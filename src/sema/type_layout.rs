//! Derived type memory layout computation.
//!
//! Computes field offsets, alignment, and total size for derived types
//! using natural alignment rules (C struct layout; sizes and
//! alignments come from `TargetLayout`, identical across the LP64
//! targets we support).

use super::symtab::{Access, ScopeId, TypeInfo};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

const TYPE_TAG_VALUE_MASK: u64 = i64::MAX as u64;

/// Procedure-pointer components carry a code pointer followed by a small
/// fixed closure payload for contained-procedure host references.
pub const PROC_PTR_CLOSURE_SLOTS: usize = 8;
pub const PROC_PTR_COMPONENT_SIZE: usize = 8 * (1 + PROC_PTR_CLOSURE_SLOTS);

/// Sprint 07: borrow when the input is already canonical lowercase,
/// allocate only when at least one ASCII uppercase byte needs folding.
fn ensure_ascii_lowercase(s: &str) -> Cow<'_, str> {
    if s.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(s.to_ascii_lowercase())
    } else {
        Cow::Borrowed(s)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldDefaultInit {
    Character(String),
    Integer(i128),
    Logical(bool),
    Real(f64),
    Derived(Vec<(String, FieldDefaultInit)>),
    /// Procedure pointer initial association from a derived-type
    /// component declared as `procedure(iface), pointer :: name =>
    /// target_proc`.  Stores the target procedure name; lowering
    /// resolves it to a function reference at construction time and
    /// stores the address in the field slot (8 bytes).  Without this,
    /// `instance%name(...)` calls jumped through whatever bytes
    /// happened to land in the slot — the immediate motivator was
    /// stdlib_hashmaps's `hasher => default_hasher` field.
    ProcedurePointer(String),
}

pub fn derived_param_field_lookup_key(base: &str, field: &str) -> String {
    format!("{}.{}", base.to_lowercase(), field.to_lowercase())
}

/// Layout of a single field in a derived type.
#[derive(Debug, Clone)]
pub struct FieldLayout {
    pub name: String,
    /// Source accessibility of this component. Unlike the enclosing type's
    /// accessibility, this controls component selection and structure
    /// constructor arguments after the type itself has been made visible.
    pub access: Access,
    /// Module that declared this component. Inherited fields retain their
    /// original owner so extending modules cannot acquire access merely by
    /// cloning the parent's physical layout.
    pub owner_module: Option<String>,
    pub offset: usize,
    pub size: usize,
    pub dims: Vec<(i64, i64)>,
    pub declared_array: bool,
    pub type_info: TypeInfo,
    /// F2018 §7.5.4.5 / §7.5.4.6 attributes on the component, carried
    /// per-field because validation of `obj%comp` as an ALLOCATE
    /// target or pointer-assignment LHS/RHS needs the leaf
    /// component's attributes, not the base variable's.
    pub allocatable: bool,
    pub pointer: bool,
    pub deferred_char: bool,
    pub target: bool,
    pub procedure_pointer: bool,
    pub procedure_pointer_nopass: bool,
    pub default_init: Option<FieldDefaultInit>,
}

/// A type-bound procedure mapping.
#[derive(Debug, Clone)]
pub struct BoundProc {
    pub method_name: String,
    pub target_name: String,
    pub abi_name: String,
    pub nopass: bool,
}

/// Complete layout of a derived type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalProc {
    pub name: String,
    pub rank: usize,
    /// Whether a rank-zero procedure may be applied element-wise to arrays.
    pub elemental: bool,
}

#[derive(Debug, Clone)]
pub struct TypeLayout {
    pub name: String,
    pub owner_module: Option<String>,
    /// Resolver scope that owns this declaration. This is compilation-local
    /// metadata and is intentionally not serialized into `.amod` files.
    pub owner_scope: Option<ScopeId>,
    /// Stable lexical path for the owner scope. Module-owned types use the
    /// lowercase module name; local types append their lexical scopes.
    pub owner_path: Option<String>,
    pub size: usize,
    pub align: usize,
    pub fields: Vec<FieldLayout>,
    pub bound_procs: Vec<BoundProc>,
    pub final_procs: Vec<FinalProc>,
    /// Unique type tag for polymorphic dispatch. Assigned sequentially.
    pub type_tag: u64,
    /// Parent type name (from EXTENDS). None for base types.
    pub parent: Option<String>,
    /// Whether this type is ABSTRACT and therefore not a concrete dispatch target.
    pub is_abstract: bool,
}

impl TypeLayout {
    /// Look up a field by name and return its layout.
    ///
    /// Sprint 07: switched from per-iteration `to_lowercase()` to
    /// `eq_ignore_ascii_case`. The original form allocated one
    /// `String` for the query and one per field comparison; this form
    /// allocates none. For the typical 5-15 field count the linear
    /// scan stays the dominant cost — a HashMap-based field index
    /// (full Sprint 07 scope) will land in a follow-up.
    pub fn field(&self, name: &str) -> Option<&FieldLayout> {
        self.fields
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case(name))
    }

    /// Look up a type-bound procedure by method name.
    pub fn bound_proc(&self, name: &str) -> Option<&BoundProc> {
        self.bound_procs
            .iter()
            .find(|p| p.method_name.eq_ignore_ascii_case(name))
    }

    pub fn bound_proc_candidates(&self, name: &str) -> Vec<&BoundProc> {
        self.bound_procs
            .iter()
            .filter(|p| p.method_name.eq_ignore_ascii_case(name))
            .collect()
    }
}

/// Return the physical field targeted by a structure-constructor argument.
///
/// Extended types expose their immediate parent as one whole component in a
/// constructor even though `TypeLayout::fields` stores the inherited fields
/// flattened at the start of the child's physical layout. When the first
/// positional argument has the parent's type, arguments map to
/// `[whole parent, own fields...]`. Otherwise they map to the ordinary
/// flattened source-component sequence `[inherited fields..., own fields...]`.
/// A keyword matching the parent type names that same whole-parent view.
pub fn structure_constructor_field<'a>(
    layout: &'a TypeLayout,
    registry: &'a TypeLayoutRegistry,
    keyword: Option<&str>,
    positional_index: usize,
    positional_parent: bool,
) -> Option<Cow<'a, FieldLayout>> {
    let parent_name = layout.parent.as_deref();
    let parent_layout = parent_name.and_then(|name| registry.get_related(layout, name));

    if let Some(keyword) = keyword {
        if let Some(parent) = parent_layout {
            if parent_name.is_some_and(|name| name.eq_ignore_ascii_case(keyword))
                || parent.name.eq_ignore_ascii_case(keyword)
            {
                return Some(Cow::Owned(whole_parent_component_field(parent)));
            }
        }
        // Preserve existing diagnostics for invalid inherited-component
        // keywords; general constructor-shape validation owns rejecting them.
        return layout.field(keyword).map(Cow::Borrowed);
    }

    let Some(parent) = parent_layout else {
        return layout.fields.get(positional_index).map(Cow::Borrowed);
    };
    if !positional_parent {
        return layout.fields.get(positional_index).map(Cow::Borrowed);
    }
    if positional_index == 0 {
        return Some(Cow::Owned(whole_parent_component_field(parent)));
    }
    let inherited = parent.fields.len().min(layout.fields.len());
    let own_index = positional_index.checked_sub(1)?;
    layout.fields.get(inherited + own_index).map(Cow::Borrowed)
}

/// Decide whether the first positional constructor argument supplies the
/// whole parent component or whether positional association uses the
/// individual inherited components followed by the child's own fields.
pub fn structure_constructor_uses_positional_parent(
    layout: &TypeLayout,
    registry: &TypeLayoutRegistry,
    first_positional_type: Option<&TypeInfo>,
) -> bool {
    let Some(parent) = layout
        .parent
        .as_deref()
        .and_then(|name| registry.get_related(layout, name))
    else {
        return false;
    };
    let Some(TypeInfo::Derived(actual_name) | TypeInfo::Class(actual_name)) = first_positional_type
    else {
        return false;
    };
    registry
        .get_related(layout, actual_name)
        .or_else(|| registry.get(actual_name))
        .is_some_and(|actual| registry.is_same_or_extension_of(actual, parent))
}

/// Materialize the implicit whole-parent component at offset zero.
pub fn whole_parent_component_field(parent: &TypeLayout) -> FieldLayout {
    FieldLayout {
        name: parent.name.clone(),
        access: Access::Public,
        owner_module: parent.owner_module.clone(),
        offset: 0,
        size: parent.size,
        dims: vec![],
        declared_array: false,
        type_info: TypeInfo::Derived(parent.name.clone()),
        allocatable: false,
        pointer: false,
        deferred_char: false,
        target: false,
        procedure_pointer: false,
        procedure_pointer_nopass: false,
        default_init: None,
    }
}

/// Registry of all computed type layouts.
#[derive(Debug, Default)]
pub struct TypeLayoutRegistry {
    pub layouts: HashMap<String, TypeLayout>,
    layout_aliases: HashMap<(ScopeId, String), (ScopeId, String)>,
    bare_use_scopes: HashMap<ScopeId, Vec<ScopeId>>,
    scope_parents: HashMap<ScopeId, Option<ScopeId>>,
    scope_paths: HashMap<ScopeId, String>,
    unique_scoped_names: HashMap<String, Option<String>>,
    unique_aliases: HashMap<String, Option<String>>,
    next_tag: u64,
}

fn stable_type_tag_key(layout: &TypeLayout) -> String {
    layout_identity_key(layout)
}

fn layout_identity_key(layout: &TypeLayout) -> String {
    let name = layout.name.to_lowercase();
    if let Some(owner_path) = layout.owner_path.as_deref() {
        return format!("{}::{}", owner_path.to_lowercase(), name);
    }
    if let Some(owner_module) = layout.owner_module.as_deref() {
        return format!("{}::{}", owner_module.to_lowercase(), name);
    }
    name
}

fn record_unique_binding(
    index: &mut HashMap<String, Option<String>>,
    name: String,
    canonical: String,
) {
    match index.entry(name) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(Some(canonical));
        }
        std::collections::hash_map::Entry::Occupied(mut entry) => {
            if entry.get().as_deref() != Some(canonical.as_str()) {
                entry.insert(None);
            }
        }
    }
}

fn stable_type_tag(layout: &TypeLayout) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in stable_type_tag_key(layout).bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    // The vtable ABI reserves the high bit of its tag word for lifecycle
    // metadata. Descriptor-visible type tags remain in the low 63 bits.
    hash &= TYPE_TAG_VALUE_MASK;
    if hash == 0 {
        1
    } else {
        hash
    }
}

impl TypeLayoutRegistry {
    pub fn new() -> Self {
        Self {
            layouts: HashMap::new(),
            layout_aliases: HashMap::new(),
            bare_use_scopes: HashMap::new(),
            scope_parents: HashMap::new(),
            scope_paths: HashMap::new(),
            unique_scoped_names: HashMap::new(),
            unique_aliases: HashMap::new(),
            next_tag: 1,
        }
    }

    pub fn alloc_tag(&mut self) -> u64 {
        let tag = self.next_tag;
        self.next_tag += 1;
        tag
    }

    pub fn get(&self, type_name: &str) -> Option<&TypeLayout> {
        let key = ensure_ascii_lowercase(type_name);
        if let Some(layout) = self.layouts.get(key.as_ref()) {
            return Some(layout);
        }
        let canonical = self
            .unique_aliases
            .get(key.as_ref())
            .or_else(|| self.unique_scoped_names.get(key.as_ref()))?
            .as_deref()?;
        self.layouts.get(canonical)
    }

    pub fn get_in_module(&self, module: &str, type_name: &str) -> Option<&TypeLayout> {
        let key = format!(
            "{}::{}",
            module.to_ascii_lowercase(),
            type_name.to_ascii_lowercase()
        );
        self.layouts.get(&key)
    }

    pub fn canonical_name_for_scope(&self, scope: ScopeId, type_name: &str) -> Option<String> {
        let mut seen = HashSet::new();
        self.resolve_name_in_scope(scope, &type_name.to_ascii_lowercase(), &mut seen)
            .or_else(|| {
                let key = type_name.to_ascii_lowercase();
                self.layouts
                    .contains_key(&key)
                    .then_some(key.clone())
                    .or_else(|| {
                        self.unique_aliases
                            .get(&key)
                            .or_else(|| self.unique_scoped_names.get(&key))
                            .and_then(Clone::clone)
                    })
            })
    }

    pub fn get_for_scope(&self, scope: ScopeId, type_name: &str) -> Option<&TypeLayout> {
        let canonical = self.canonical_name_for_scope(scope, type_name)?;
        self.layouts.get(&canonical)
    }

    pub fn get_related(&self, owner: &TypeLayout, type_name: &str) -> Option<&TypeLayout> {
        owner
            .owner_scope
            .and_then(|scope| self.get_for_scope(scope, type_name))
            .or_else(|| {
                owner
                    .owner_module
                    .as_deref()
                    .and_then(|module| self.get_in_module(module, type_name))
            })
            .or_else(|| self.get(type_name))
    }

    pub fn is_same_or_extension_of(&self, actual: &TypeLayout, declared: &TypeLayout) -> bool {
        let declared_key = self.canonical_key_for_layout(declared);
        let mut current = actual;
        let mut visited = HashSet::new();
        loop {
            let current_key = self.canonical_key_for_layout(current);
            if current_key == declared_key {
                return true;
            }
            if !visited.insert(current_key) {
                return false;
            }
            let Some(parent) = current
                .parent
                .as_deref()
                .and_then(|name| self.get_related(current, name))
            else {
                return false;
            };
            current = parent;
        }
    }

    pub fn canonical_key_for_layout(&self, layout: &TypeLayout) -> String {
        layout_identity_key(layout)
    }

    pub fn scope_path(&self, scope: ScopeId) -> Option<&str> {
        self.scope_paths.get(&scope).map(String::as_str)
    }

    pub fn iter_layouts(&self) -> impl Iterator<Item = &TypeLayout> {
        self.layouts.values()
    }

    pub fn iter_layouts_mut(&mut self) -> impl Iterator<Item = &mut TypeLayout> {
        self.layouts.values_mut()
    }

    pub fn register_scope(&mut self, scope: ScopeId, parent: Option<ScopeId>, stable_path: String) {
        self.scope_parents.insert(scope, parent);
        self.scope_paths
            .insert(scope, stable_path.to_ascii_lowercase());
    }

    pub fn bind_layout(
        &mut self,
        scope: ScopeId,
        local_name: &str,
        source_scope: ScopeId,
        original_name: &str,
    ) {
        if local_name.is_empty() || original_name.is_empty() {
            return;
        }
        self.layout_aliases.insert(
            (scope, local_name.to_ascii_lowercase()),
            (source_scope, original_name.to_ascii_lowercase()),
        );
    }

    pub fn bind_bare_use_scope(&mut self, scope: ScopeId, source_scope: ScopeId) {
        let sources = self.bare_use_scopes.entry(scope).or_default();
        if !sources.contains(&source_scope) {
            sources.push(source_scope);
        }
    }

    pub fn rebuild_alias_index(&mut self) {
        self.unique_aliases.clear();
        let bindings: Vec<_> = self
            .layout_aliases
            .iter()
            .map(|((scope, local), (source_scope, original))| {
                (*scope, local.clone(), *source_scope, original.clone())
            })
            .collect();
        for (_scope, local, source_scope, original) in bindings {
            let mut seen = HashSet::new();
            if let Some(canonical) = self.resolve_name_in_scope(source_scope, &original, &mut seen)
            {
                record_unique_binding(&mut self.unique_aliases, local, canonical);
            }
        }
    }

    pub fn has_inline_storage_cycle(&self, layout: &TypeLayout) -> bool {
        fn walk(
            registry: &TypeLayoutRegistry,
            layout: &TypeLayout,
            visiting: &mut HashSet<String>,
            complete: &mut HashSet<String>,
        ) -> bool {
            let key = registry.canonical_key_for_layout(layout);
            if visiting.contains(&key) {
                return true;
            }
            if complete.contains(&key) {
                return false;
            }
            visiting.insert(key.clone());

            let parent_cycle = layout
                .parent
                .as_deref()
                .and_then(|name| registry.get_related(layout, name))
                .is_some_and(|parent| walk(registry, parent, visiting, complete));
            let field_cycle = layout.fields.iter().any(|field| {
                if field.pointer || field.allocatable {
                    return false;
                }
                let TypeInfo::Derived(name) = &field.type_info else {
                    return false;
                };
                let nested =
                    if name.eq_ignore_ascii_case(&layout.name) || name.eq_ignore_ascii_case(&key) {
                        Some(layout)
                    } else {
                        registry.get_related(layout, name)
                    };
                nested.is_some_and(|nested| walk(registry, nested, visiting, complete))
            });

            visiting.remove(&key);
            complete.insert(key);
            parent_cycle || field_cycle
        }

        walk(self, layout, &mut HashSet::new(), &mut HashSet::new())
    }

    pub fn lifecycle_has_ownerless_finalizer(&self, layout: &TypeLayout) -> bool {
        fn walk(
            registry: &TypeLayoutRegistry,
            layout: &TypeLayout,
            visited: &mut HashSet<String>,
        ) -> bool {
            let key = registry.canonical_key_for_layout(layout);
            if !visited.insert(key) {
                return false;
            }
            if layout.owner_module.is_none() && !layout.final_procs.is_empty() {
                return true;
            }
            if layout
                .parent
                .as_deref()
                .and_then(|name| registry.get_related(layout, name))
                .is_some_and(|parent| walk(registry, parent, visited))
            {
                return true;
            }
            layout.fields.iter().any(|field| {
                if field.pointer {
                    return false;
                }
                let (TypeInfo::Derived(name) | TypeInfo::Class(name)) = &field.type_info else {
                    return false;
                };
                registry
                    .get_related(layout, name)
                    .is_some_and(|nested| walk(registry, nested, visited))
            })
        }

        walk(self, layout, &mut HashSet::new())
    }

    /// Whether a generated component-deallocation helper would need to invoke
    /// a procedure-local FINAL binding. The root object's own finalizer is not
    /// part of this walk: static callers invoke it through the host-aware path
    /// before asking the helper to release owned components.
    pub fn deallocation_has_ownerless_finalizer(&self, layout: &TypeLayout) -> bool {
        fn walk(
            registry: &TypeLayoutRegistry,
            layout: &TypeLayout,
            visited: &mut HashSet<String>,
        ) -> bool {
            let key = registry.canonical_key_for_layout(layout);
            if !visited.insert(key) {
                return false;
            }

            layout.fields.iter().any(|field| {
                if field.pointer {
                    return false;
                }
                let (TypeInfo::Derived(name) | TypeInfo::Class(name)) = &field.type_info else {
                    return false;
                };
                let Some(nested) = registry.get_related(layout, name) else {
                    return false;
                };
                if field.allocatable && registry.lifecycle_has_ownerless_finalizer(nested) {
                    return true;
                }
                walk(registry, nested, visited)
            })
        }

        walk(self, layout, &mut HashSet::new())
    }

    /// Find a visible concrete type whose lifecycle cannot be represented by
    /// the context-free vtable ABI for a runtime-polymorphic value.
    pub fn visible_ownerless_finalizer_for_polymorphic(
        &self,
        scope: ScopeId,
        declared: &TypeInfo,
    ) -> Option<&TypeLayout> {
        fn scope_can_see(
            registry: &TypeLayoutRegistry,
            scope: ScopeId,
            owner: Option<ScopeId>,
        ) -> bool {
            let Some(owner) = owner else {
                return true;
            };
            let mut current = Some(scope);
            let mut visited = HashSet::new();
            while let Some(candidate) = current {
                if candidate == owner {
                    return true;
                }
                if !visited.insert(candidate) {
                    break;
                }
                current = registry.scope_parents.get(&candidate).copied().flatten();
            }
            false
        }

        fn is_same_or_extension_of(
            registry: &TypeLayoutRegistry,
            candidate: &TypeLayout,
            base_key: &str,
            visited: &mut HashSet<String>,
        ) -> bool {
            let key = registry.canonical_key_for_layout(candidate);
            if key == base_key {
                return true;
            }
            if !visited.insert(key) {
                return false;
            }
            candidate
                .parent
                .as_deref()
                .and_then(|name| registry.get_related(candidate, name))
                .is_some_and(|parent| is_same_or_extension_of(registry, parent, base_key, visited))
        }

        let base_key = match declared {
            TypeInfo::Class(name) => self
                .get_for_scope(scope, name)
                .or_else(|| self.get(name))
                .map(|layout| self.canonical_key_for_layout(layout)),
            TypeInfo::ClassStar => None,
            _ => return None,
        };

        let mut candidates: Vec<_> = self
            .layouts
            .values()
            .filter(|candidate| {
                if candidate.is_abstract
                    || candidate.owner_module.is_some()
                    || !scope_can_see(self, scope, candidate.owner_scope)
                    || !self.lifecycle_has_ownerless_finalizer(candidate)
                {
                    return false;
                }
                base_key.as_deref().is_none_or(|base| {
                    is_same_or_extension_of(self, candidate, base, &mut HashSet::new())
                })
            })
            .collect();
        candidates.sort_by_key(|candidate| self.canonical_key_for_layout(candidate));
        candidates.into_iter().next()
    }

    fn resolve_name_in_scope(
        &self,
        scope: ScopeId,
        name: &str,
        seen: &mut HashSet<(ScopeId, String)>,
    ) -> Option<String> {
        let visit = (scope, name.to_string());
        if !seen.insert(visit.clone()) {
            return None;
        }

        if let Some(path) = self.scope_paths.get(&scope) {
            let canonical = format!("{}::{}", path, name);
            if self.layouts.contains_key(&canonical) {
                seen.remove(&visit);
                return Some(canonical);
            }
        }

        if let Some((source_scope, original_name)) =
            self.layout_aliases.get(&(scope, name.to_string()))
        {
            let resolved = self.resolve_name_in_scope(*source_scope, original_name, seen);
            seen.remove(&visit);
            return resolved;
        }

        let mut imported = None;
        if let Some(source_scopes) = self.bare_use_scopes.get(&scope) {
            for source_scope in source_scopes {
                let mut branch_seen = seen.clone();
                let Some(candidate) =
                    self.resolve_name_in_scope(*source_scope, name, &mut branch_seen)
                else {
                    continue;
                };
                match imported.as_deref() {
                    None => imported = Some(candidate),
                    Some(existing) if existing == candidate => {}
                    Some(_) => {
                        seen.remove(&visit);
                        return None;
                    }
                }
            }
        }
        if imported.is_some() {
            seen.remove(&visit);
            return imported;
        }

        let resolved = self
            .scope_parents
            .get(&scope)
            .copied()
            .flatten()
            .and_then(|parent| self.resolve_name_in_scope(parent, name, seen));
        seen.remove(&visit);
        resolved
    }

    pub fn insert(&mut self, mut layout: TypeLayout) {
        if layout.type_tag == 0 {
            layout.type_tag = stable_type_tag(&layout);
        } else {
            layout.type_tag &= TYPE_TAG_VALUE_MASK;
            if layout.type_tag == 0 {
                layout.type_tag = 1;
            }
            if layout.type_tag < TYPE_TAG_VALUE_MASK && layout.type_tag >= self.next_tag {
                self.next_tag = layout.type_tag + 1;
            }
        }
        let name = layout.name.to_lowercase();
        let canonical = layout_identity_key(&layout);
        self.layouts.insert(canonical.clone(), layout);
        record_unique_binding(&mut self.unique_scoped_names, name, canonical);
    }
}

/// Size and alignment for types whose footprint derives entirely from
/// the Fortran KIND (or character length) — no target-layout input.
/// Returns `None` for Derived/Class/unlimited-polymorphic entities,
/// whose size is a pointer or descriptor and therefore a
/// `TargetLayout` question. Callers that have already excluded those
/// variants (e.g. `type_info_to_ir_type`) use this directly instead of
/// threading a layout they cannot consume.
pub fn size_of_scalar_kind(ti: &TypeInfo) -> Option<(usize, usize)> {
    match ti {
        // Enumeration values are default-integer ordinals.
        TypeInfo::Enumeration(_) => Some((4, 4)),
        TypeInfo::Integer { kind } => {
            // No explicit kind selector → honour the driver's
            // -fdefault-integer-8 (sprint 32 #504).  Standard
            // processor default is 4.
            let k = kind
                .map(|k| k as usize)
                .unwrap_or_else(|| crate::driver::defaults::default_int_kind() as usize);
            Some((k, k))
        }
        TypeInfo::Real { kind } => {
            let k = kind
                .map(|k| k as usize)
                .unwrap_or_else(|| crate::driver::defaults::default_real_kind() as usize);
            Some((k, k))
        }
        TypeInfo::DoublePrecision => Some((8, 8)),
        TypeInfo::Complex { kind } => {
            let k = kind.unwrap_or(4) as usize;
            Some((k * 2, k)) // complex(4) = 8 bytes, aligned to 4
        }
        TypeInfo::Logical { kind } => {
            let k = kind.unwrap_or(4) as usize;
            Some((k, k))
        }
        TypeInfo::Character { len, kind: _ } => {
            let l = len.unwrap_or(1) as usize;
            Some((l, 1))
        }
        TypeInfo::Derived(_) | TypeInfo::Class(_) | TypeInfo::ClassStar | TypeInfo::TypeStar => {
            None
        }
    }
}

/// Compute the size and alignment of a Fortran type under the given
/// target layout.
pub fn size_of_type(ti: &TypeInfo, layout: crate::target::TargetLayout) -> (usize, usize) {
    if let Some(scalar) = size_of_scalar_kind(ti) {
        return scalar;
    }
    match ti {
        TypeInfo::Derived(_) => (layout.ptr_bytes, layout.ptr_align), // resolved by caller via registry
        TypeInfo::Class(_) | TypeInfo::ClassStar | TypeInfo::TypeStar => layout.class_descriptor(),
        _ => unreachable!("scalar kinds handled by size_of_scalar_kind"),
    }
}

fn eval_const_int_expr(
    expr: &crate::ast::expr::SpannedExpr,
    const_params: &HashMap<String, i64>,
) -> Option<i64> {
    use crate::ast::expr::Expr;
    match &expr.node {
        Expr::IntegerLiteral { text, .. } => {
            let clean = text.split('_').next().unwrap_or(text);
            clean.parse::<i64>().ok()
        }
        Expr::Name { name } => const_params.get(&name.to_lowercase()).copied(),
        Expr::UnaryOp { op, operand } => {
            let v = eval_const_int_expr(operand, const_params)?;
            match op {
                crate::ast::expr::UnaryOp::Minus => Some(-v),
                crate::ast::expr::UnaryOp::Plus => Some(v),
                _ => None,
            }
        }
        Expr::BinaryOp { op, left, right } => {
            let l = eval_const_int_expr(left, const_params)?;
            let r = eval_const_int_expr(right, const_params)?;
            match op {
                crate::ast::expr::BinaryOp::Add => Some(l + r),
                crate::ast::expr::BinaryOp::Sub => Some(l - r),
                crate::ast::expr::BinaryOp::Mul => Some(l * r),
                crate::ast::expr::BinaryOp::Div if r != 0 => Some(l / r),
                _ => None,
            }
        }
        Expr::ParenExpr { inner } => eval_const_int_expr(inner, const_params),
        Expr::FunctionCall { callee, args } => {
            let Expr::Name { name } = &callee.node else {
                return None;
            };
            let first_arg_val = args.first().and_then(|a| {
                if let crate::ast::expr::SectionSubscript::Element(e) = &a.value {
                    eval_const_int_expr(e, const_params)
                } else {
                    None
                }
            });
            match name.to_lowercase().as_str() {
                "selected_int_kind" => {
                    let r = first_arg_val?;
                    Some(if r <= 2 {
                        1
                    } else if r <= 4 {
                        2
                    } else if r <= 9 {
                        4
                    } else if r <= 18 {
                        8
                    } else if r <= 38 {
                        16
                    } else {
                        -1
                    })
                }
                "selected_real_kind" => {
                    let p = first_arg_val?;
                    Some(if p <= 6 {
                        4
                    } else if p <= 15 {
                        8
                    } else {
                        -1
                    })
                }
                "selected_logical_kind" => {
                    let bits = first_arg_val?;
                    Some(if bits <= 8 {
                        1
                    } else if bits <= 16 {
                        2
                    } else if bits <= 32 {
                        4
                    } else if bits <= 64 {
                        8
                    } else if bits <= 128 {
                        16
                    } else {
                        -1
                    })
                }
                "selected_char_kind" => {
                    let arg = args.first()?;
                    let crate::ast::expr::SectionSubscript::Element(e) = &arg.value else {
                        return None;
                    };
                    if let Expr::StringLiteral { value, .. } = &e.node {
                        let bytes = value.as_bytes();
                        let name = std::str::from_utf8(bytes.as_ref())
                            .ok()?
                            .trim_end_matches(' ');
                        Some(
                            if name.eq_ignore_ascii_case("default")
                                || name.eq_ignore_ascii_case("ascii")
                            {
                                1
                            } else {
                                -1
                            },
                        )
                    } else {
                        None
                    }
                }
                "kind" => {
                    let arg = args.first()?;
                    let crate::ast::expr::SectionSubscript::Element(e) = &arg.value else {
                        return None;
                    };
                    match &e.node {
                        Expr::RealLiteral { text, .. } => {
                            Some(if text.contains('d') || text.contains('D') {
                                8
                            } else {
                                4
                            })
                        }
                        Expr::IntegerLiteral { .. } => Some(4),
                        _ => None,
                    }
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn eval_const_logical_expr(expr: &crate::ast::expr::SpannedExpr) -> Option<bool> {
    use crate::ast::expr::Expr;
    match &expr.node {
        Expr::LogicalLiteral { value, .. } => Some(*value),
        Expr::ParenExpr { inner } => eval_const_logical_expr(inner),
        Expr::UnaryOp {
            op: crate::ast::expr::UnaryOp::Not,
            operand,
        } => eval_const_logical_expr(operand).map(|v| !v),
        _ => None,
    }
}

fn eval_const_field_int_expr(
    expr: &crate::ast::expr::SpannedExpr,
    const_params: &HashMap<String, i64>,
    const_derived_field_inits: &HashMap<String, FieldDefaultInit>,
) -> Option<i64> {
    use crate::ast::expr::Expr;
    match &expr.node {
        Expr::ComponentAccess { base, component } => {
            let Expr::Name { name } = &base.node else {
                return None;
            };
            match const_derived_field_inits.get(&derived_param_field_lookup_key(name, component)) {
                Some(FieldDefaultInit::Integer(value)) => i64::try_from(*value).ok(),
                _ => None,
            }
        }
        Expr::ParenExpr { inner } => {
            eval_const_field_int_expr(inner, const_params, const_derived_field_inits)
        }
        Expr::UnaryOp { op, operand } => {
            let value =
                eval_const_field_int_expr(operand, const_params, const_derived_field_inits)?;
            match op {
                crate::ast::expr::UnaryOp::Minus => Some(-value),
                crate::ast::expr::UnaryOp::Plus => Some(value),
                _ => None,
            }
        }
        Expr::BinaryOp { op, left, right } => {
            let lhs = eval_const_field_int_expr(left, const_params, const_derived_field_inits)?;
            let rhs = eval_const_field_int_expr(right, const_params, const_derived_field_inits)?;
            match op {
                crate::ast::expr::BinaryOp::Add => Some(lhs + rhs),
                crate::ast::expr::BinaryOp::Sub => Some(lhs - rhs),
                crate::ast::expr::BinaryOp::Mul => Some(lhs * rhs),
                crate::ast::expr::BinaryOp::Div if rhs != 0 => Some(lhs / rhs),
                _ => None,
            }
        }
        _ => eval_const_int_expr(expr, const_params),
    }
}

fn eval_const_field_logical_expr(
    expr: &crate::ast::expr::SpannedExpr,
    const_derived_field_inits: &HashMap<String, FieldDefaultInit>,
) -> Option<bool> {
    use crate::ast::expr::Expr;
    match &expr.node {
        Expr::ComponentAccess { base, component } => {
            let Expr::Name { name } = &base.node else {
                return None;
            };
            match const_derived_field_inits.get(&derived_param_field_lookup_key(name, component)) {
                Some(FieldDefaultInit::Logical(value)) => Some(*value),
                _ => None,
            }
        }
        Expr::ParenExpr { inner } => {
            eval_const_field_logical_expr(inner, const_derived_field_inits)
        }
        Expr::UnaryOp {
            op: crate::ast::expr::UnaryOp::Not,
            operand,
        } => eval_const_field_logical_expr(operand, const_derived_field_inits).map(|value| !value),
        _ => eval_const_logical_expr(expr),
    }
}

fn eval_const_field_real_expr(
    expr: &crate::ast::expr::SpannedExpr,
    const_params: &HashMap<String, i64>,
    const_derived_field_inits: &HashMap<String, FieldDefaultInit>,
) -> Option<f64> {
    use crate::ast::expr::Expr;
    match &expr.node {
        Expr::RealLiteral { text, .. } => text
            .replace('d', "e")
            .replace('D', "E")
            .split('_')
            .next()
            .unwrap_or(text)
            .parse::<f64>()
            .ok(),
        Expr::IntegerLiteral { .. } => eval_const_int_expr(expr, const_params).map(|v| v as f64),
        Expr::Name { name } => const_params.get(&name.to_lowercase()).map(|v| *v as f64),
        Expr::ComponentAccess { base, component } => {
            let Expr::Name { name } = &base.node else {
                return None;
            };
            match const_derived_field_inits.get(&derived_param_field_lookup_key(name, component)) {
                Some(FieldDefaultInit::Real(value)) => Some(*value),
                Some(FieldDefaultInit::Integer(value)) => Some(*value as f64),
                _ => None,
            }
        }
        Expr::ParenExpr { inner } => {
            eval_const_field_real_expr(inner, const_params, const_derived_field_inits)
        }
        Expr::UnaryOp { op, operand } => {
            let value =
                eval_const_field_real_expr(operand, const_params, const_derived_field_inits)?;
            match op {
                crate::ast::expr::UnaryOp::Minus => Some(-value),
                crate::ast::expr::UnaryOp::Plus => Some(value),
                _ => None,
            }
        }
        Expr::BinaryOp { op, left, right } => {
            let lhs = eval_const_field_real_expr(left, const_params, const_derived_field_inits)?;
            let rhs = eval_const_field_real_expr(right, const_params, const_derived_field_inits)?;
            match op {
                crate::ast::expr::BinaryOp::Add => Some(lhs + rhs),
                crate::ast::expr::BinaryOp::Sub => Some(lhs - rhs),
                crate::ast::expr::BinaryOp::Mul => Some(lhs * rhs),
                crate::ast::expr::BinaryOp::Div if rhs != 0.0 => Some(lhs / rhs),
                _ => None,
            }
        }
        _ => None,
    }
}

fn eval_const_field_char_expr(
    expr: &crate::ast::expr::SpannedExpr,
    const_params: &HashMap<String, i64>,
    const_char_params: &HashMap<String, String>,
    const_derived_field_inits: &HashMap<String, FieldDefaultInit>,
) -> Option<String> {
    use crate::ast::expr::Expr;

    match &expr.node {
        Expr::StringLiteral { value, .. } => Some(value.source_view().into_owned()),
        Expr::Name { name } => const_char_params.get(&name.to_lowercase()).cloned(),
        Expr::ComponentAccess { base, component } => {
            let Expr::Name { name } = &base.node else {
                return None;
            };
            match const_derived_field_inits.get(&derived_param_field_lookup_key(name, component)) {
                Some(FieldDefaultInit::Character(value)) => Some(value.clone()),
                _ => None,
            }
        }
        Expr::ParenExpr { inner } => eval_const_field_char_expr(
            inner,
            const_params,
            const_char_params,
            const_derived_field_inits,
        ),
        Expr::BinaryOp {
            op: crate::ast::expr::BinaryOp::Concat,
            left,
            right,
        } => {
            let mut out = eval_const_field_char_expr(
                left,
                const_params,
                const_char_params,
                const_derived_field_inits,
            )?;
            out.push_str(&eval_const_field_char_expr(
                right,
                const_params,
                const_char_params,
                const_derived_field_inits,
            )?);
            Some(out)
        }
        Expr::FunctionCall { callee, args } => {
            let Expr::Name { name } = &callee.node else {
                return None;
            };
            match name.to_lowercase().as_str() {
                "char" | "achar" => {
                    let first_arg = args.first().and_then(|arg| {
                        if let crate::ast::expr::SectionSubscript::Element(expr) = &arg.value {
                            Some(expr)
                        } else {
                            None
                        }
                    })?;
                    let code = eval_const_field_int_expr(
                        first_arg,
                        const_params,
                        const_derived_field_inits,
                    )?;
                    if !(0..=255).contains(&code) {
                        return None;
                    }
                    Some((code as u8 as char).to_string())
                }
                "new_line" => Some("\n".to_string()),
                "repeat" if args.len() == 2 => {
                    let pattern = match &args[0].value {
                        crate::ast::expr::SectionSubscript::Element(expr) => {
                            eval_const_field_char_expr(
                                expr,
                                const_params,
                                const_char_params,
                                const_derived_field_inits,
                            )?
                        }
                        _ => return None,
                    };
                    let copies = match &args[1].value {
                        crate::ast::expr::SectionSubscript::Element(expr) => {
                            eval_const_field_int_expr(
                                expr,
                                const_params,
                                const_derived_field_inits,
                            )?
                        }
                        _ => return None,
                    };
                    if copies < 0 {
                        return None;
                    }
                    Some(pattern.repeat(copies as usize))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

pub fn eval_const_field_default_init_for_layout(
    type_info: &TypeInfo,
    expr: &crate::ast::expr::SpannedExpr,
    registry: &TypeLayoutRegistry,
    const_params: &HashMap<String, i64>,
    const_char_params: &HashMap<String, String>,
    const_derived_field_inits: &HashMap<String, FieldDefaultInit>,
) -> Option<FieldDefaultInit> {
    match type_info {
        TypeInfo::Character { .. } => eval_const_field_char_expr(
            expr,
            const_params,
            const_char_params,
            const_derived_field_inits,
        )
        .map(FieldDefaultInit::Character),
        TypeInfo::Integer { .. } => {
            eval_const_field_int_expr(expr, const_params, const_derived_field_inits)
                .map(|value| FieldDefaultInit::Integer(value as i128))
        }
        TypeInfo::Logical { .. } => eval_const_field_logical_expr(expr, const_derived_field_inits)
            .map(FieldDefaultInit::Logical),
        TypeInfo::Real { .. } | TypeInfo::DoublePrecision => {
            eval_const_field_real_expr(expr, const_params, const_derived_field_inits)
                .map(FieldDefaultInit::Real)
        }
        TypeInfo::Derived(type_name) | TypeInfo::Class(type_name) => {
            eval_const_derived_default_init(
                type_name,
                expr,
                registry,
                const_params,
                const_char_params,
                const_derived_field_inits,
            )
        }
        _ => None,
    }
}

fn eval_const_derived_default_init(
    type_name: &str,
    expr: &crate::ast::expr::SpannedExpr,
    registry: &TypeLayoutRegistry,
    const_params: &HashMap<String, i64>,
    const_char_params: &HashMap<String, String>,
    const_derived_field_inits: &HashMap<String, FieldDefaultInit>,
) -> Option<FieldDefaultInit> {
    use crate::ast::expr::{Expr, SectionSubscript};

    let Expr::FunctionCall { callee, args } = &expr.node else {
        return None;
    };
    let Expr::Name { name: callee_name } = &callee.node else {
        return None;
    };
    if !callee_name.eq_ignore_ascii_case(type_name) {
        return None;
    }

    let layout = registry.get(type_name)?;
    let mut positional_idx = 0usize;
    let mut overrides = Vec::new();

    for arg in args {
        let field = if let Some(keyword) = &arg.keyword {
            layout.field(keyword)?
        } else {
            let field = layout.fields.get(positional_idx)?;
            positional_idx += 1;
            field
        };
        if !field.dims.is_empty() || field.allocatable || field.pointer {
            return None;
        }
        let SectionSubscript::Element(value_expr) = &arg.value else {
            return None;
        };
        let init = eval_const_field_default_init_for_layout(
            &field.type_info,
            value_expr,
            registry,
            const_params,
            const_char_params,
            const_derived_field_inits,
        )?;
        overrides.push((field.name.clone(), init));
    }

    Some(FieldDefaultInit::Derived(overrides))
}

fn eval_explicit_array_dims(
    specs: Option<&Vec<crate::ast::decl::ArraySpec>>,
    const_params: &HashMap<String, i64>,
) -> Vec<(i64, i64)> {
    let Some(specs) = specs else {
        return Vec::new();
    };
    let mut dims = Vec::new();
    for spec in specs {
        let crate::ast::decl::ArraySpec::Explicit { lower, upper } = spec else {
            return Vec::new();
        };
        let lower_val = lower
            .as_ref()
            .and_then(|expr| eval_const_int_expr(expr, const_params))
            .unwrap_or(1);
        let upper_val = match eval_const_int_expr(upper, const_params) {
            Some(value) => value,
            None => return Vec::new(),
        };
        let extent = (upper_val - lower_val + 1).max(0);
        dims.push((lower_val, extent));
    }
    dims
}

/// Convert a TypeSpec AST node to TypeInfo for layout computation.
fn type_spec_to_type_info(
    ts: &crate::ast::decl::TypeSpec,
    const_params: &HashMap<String, i64>,
) -> TypeInfo {
    use crate::ast::decl::{KindSelector, LenSpec, TypeSpec};

    match ts {
        TypeSpec::Integer(kind) => TypeInfo::Integer {
            kind: kind.as_ref().and_then(|k| match k {
                KindSelector::Expr(e) | KindSelector::Star(e) => {
                    eval_const_int_expr(e, const_params).and_then(|v| u8::try_from(v).ok())
                }
            }),
        },
        TypeSpec::Real(kind) => TypeInfo::Real {
            kind: kind.as_ref().and_then(|k| match k {
                KindSelector::Expr(e) | KindSelector::Star(e) => {
                    eval_const_int_expr(e, const_params).and_then(|v| u8::try_from(v).ok())
                }
            }),
        },
        TypeSpec::DoublePrecision => TypeInfo::DoublePrecision,
        TypeSpec::Complex(kind) => TypeInfo::Complex {
            kind: kind.as_ref().and_then(|k| match k {
                KindSelector::Expr(e) | KindSelector::Star(e) => {
                    eval_const_int_expr(e, const_params).and_then(|v| u8::try_from(v).ok())
                }
            }),
        },
        TypeSpec::DoubleComplex => TypeInfo::Complex { kind: Some(8) },
        TypeSpec::Logical(kind) => TypeInfo::Logical {
            kind: kind.as_ref().and_then(|k| match k {
                KindSelector::Expr(e) | KindSelector::Star(e) => {
                    eval_const_int_expr(e, const_params).and_then(|v| u8::try_from(v).ok())
                }
            }),
        },
        TypeSpec::Character(sel) => {
            let len = sel
                .as_ref()
                .and_then(|s| s.len.as_ref())
                .and_then(|l| match l {
                    LenSpec::Expr(e) => eval_const_int_expr(e, const_params),
                    _ => None,
                });
            let kind = sel
                .as_ref()
                .and_then(|s| s.kind.as_ref())
                .and_then(|e| eval_const_int_expr(e, const_params))
                .and_then(|v| u8::try_from(v).ok());
            TypeInfo::Character { len, kind }
        }
        TypeSpec::Type(name)
            if crate::sema::resolve::type_resolution::ieee_opaque_int_kind(name).is_some() =>
        {
            // IEEE opaque types are integer under the hood (l09); a
            // derived-type component of one gets integer storage.
            TypeInfo::Integer {
                kind: crate::sema::resolve::type_resolution::ieee_opaque_int_kind(name),
            }
        }
        TypeSpec::Type(name) => TypeInfo::Derived(name.clone()),
        TypeSpec::Class(name) => TypeInfo::Class(name.clone()),
        TypeSpec::ClassStar => TypeInfo::ClassStar,
        TypeSpec::TypeStar => TypeInfo::TypeStar,
        // TYPEOF/CLASSOF in derived-type components are an l03
        // deferral (this context has no symbol table to resolve the
        // entity); validation rejects them before layout matters.
        TypeSpec::TypeOf(_) | TypeSpec::ClassOf(_) => TypeInfo::TypeStar,
    }
}

fn entity_char_len_type_info(
    mut ti: TypeInfo,
    entity_len: Option<&crate::ast::decl::LenSpec>,
    const_params: &HashMap<String, i64>,
) -> TypeInfo {
    let Some(entity_len) = entity_len else {
        return ti;
    };
    if let TypeInfo::Character { len, .. } = &mut ti {
        *len = match entity_len {
            crate::ast::decl::LenSpec::Expr(e) => eval_const_int_expr(e, const_params),
            crate::ast::decl::LenSpec::Star | crate::ast::decl::LenSpec::Colon => None,
        };
    }
    ti
}

/// Compute the layout of a derived type from its component declarations.
pub fn compute_layout(
    type_name: &str,
    host_module: Option<&str>,
    type_bound_procs: &[crate::ast::decl::TypeBoundProc],
    final_proc_names: &[String],
    components: &[crate::ast::decl::SpannedDecl],
    parent_layout: Option<&TypeLayout>,
    registry: &TypeLayoutRegistry,
    const_params: &HashMap<String, i64>,
    layout: crate::target::TargetLayout,
) -> TypeLayout {
    let const_derived_field_inits = HashMap::new();
    let const_char_params = HashMap::new();
    compute_layout_with_attrs(
        type_name,
        host_module,
        type_bound_procs,
        final_proc_names,
        components,
        parent_layout,
        false,
        registry,
        const_params,
        &const_char_params,
        &const_derived_field_inits,
        layout,
    )
}

pub fn compute_layout_with_attrs(
    type_name: &str,
    host_module: Option<&str>,
    type_bound_procs: &[crate::ast::decl::TypeBoundProc],
    final_proc_names: &[String],
    components: &[crate::ast::decl::SpannedDecl],
    parent_layout: Option<&TypeLayout>,
    is_abstract: bool,
    registry: &TypeLayoutRegistry,
    const_params: &HashMap<String, i64>,
    const_char_params: &HashMap<String, String>,
    const_derived_field_inits: &HashMap<String, FieldDefaultInit>,
    layout: crate::target::TargetLayout,
) -> TypeLayout {
    compute_layout_with_attrs_in_scope(
        type_name,
        host_module,
        None,
        None,
        type_bound_procs,
        final_proc_names,
        components,
        parent_layout,
        is_abstract,
        registry,
        const_params,
        const_char_params,
        const_derived_field_inits,
        layout,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_layout_with_attrs_in_scope(
    type_name: &str,
    host_module: Option<&str>,
    owner_scope: Option<ScopeId>,
    owner_path: Option<&str>,
    type_bound_procs: &[crate::ast::decl::TypeBoundProc],
    final_proc_names: &[String],
    components: &[crate::ast::decl::SpannedDecl],
    parent_layout: Option<&TypeLayout>,
    is_abstract: bool,
    registry: &TypeLayoutRegistry,
    const_params: &HashMap<String, i64>,
    const_char_params: &HashMap<String, String>,
    const_derived_field_inits: &HashMap<String, FieldDefaultInit>,
    layout: crate::target::TargetLayout,
) -> TypeLayout {
    let mut offset: usize = 0;
    let mut max_align: usize = 1;
    let mut fields = Vec::new();
    let default_component_access = if components.iter().any(|decl| {
        matches!(
            decl.node,
            crate::ast::decl::Decl::AccessDefault {
                access: crate::ast::decl::Attribute::Private
            }
        )
    }) {
        Access::Private
    } else {
        Access::Public
    };

    // If this type extends a parent, start with parent's fields.
    if let Some(parent) = parent_layout {
        for f in &parent.fields {
            fields.push(f.clone());
        }
        offset = parent.size;
        max_align = parent.align;
    }

    // Process component declarations.
    for decl in components {
        if let crate::ast::decl::Decl::TypeDecl {
            type_spec,
            attrs,
            entities,
        } = &decl.node
        {
            let is_allocatable = attrs
                .iter()
                .any(|a| matches!(a, crate::ast::decl::Attribute::Allocatable));
            let is_pointer = attrs
                .iter()
                .any(|a| matches!(a, crate::ast::decl::Attribute::Pointer));
            let is_target = attrs
                .iter()
                .any(|a| matches!(a, crate::ast::decl::Attribute::Target));
            let component_access = attrs
                .iter()
                .rev()
                .find_map(|attr| match attr {
                    crate::ast::decl::Attribute::Public => Some(Access::Public),
                    crate::ast::decl::Attribute::Private => Some(Access::Private),
                    _ => None,
                })
                .unwrap_or(default_component_access);
            let dimension_attr_specs = attrs.iter().find_map(|a| {
                if let crate::ast::decl::Attribute::Dimension(specs) = a {
                    Some(specs)
                } else {
                    None
                }
            });

            let base_ti = type_spec_to_type_info(type_spec, const_params);
            for entity in entities {
                let ti = entity_char_len_type_info(
                    base_ti.clone(),
                    entity.char_len.as_ref(),
                    const_params,
                );
                let effective_char_len = entity.char_len.as_ref().or(match type_spec {
                    crate::ast::decl::TypeSpec::Character(Some(sel)) => sel.len.as_ref(),
                    _ => None,
                });
                let deferred_char =
                    matches!(effective_char_len, Some(crate::ast::decl::LenSpec::Colon));
                let explicit_array_specs = entity.array_spec.as_ref().or(dimension_attr_specs);
                let declared_rank = explicit_array_specs.map_or(0, |specs| specs.len());
                let declared_array = declared_rank > 0;
                let dims = if is_allocatable || is_pointer {
                    vec![(1, 0); declared_rank]
                } else {
                    eval_explicit_array_dims(explicit_array_specs, const_params)
                };
                let is_proc_pointer_component = is_pointer
                    && attrs
                        .iter()
                        .any(|a| matches!(a, crate::ast::decl::Attribute::External))
                    && matches!(ti, TypeInfo::Derived(_))
                    && dims.is_empty()
                    && !declared_array;
                let procedure_pointer_nopass = is_proc_pointer_component
                    && attrs
                        .iter()
                        .any(|a| matches!(a, crate::ast::decl::Attribute::NoPass));
                let (elem_size, elem_align) =
                    if matches!(&ti, TypeInfo::Character { len: None, .. })
                        && (is_allocatable || is_pointer)
                        && !declared_array
                    {
                        layout.string_descriptor() // deferred-length scalar char component
                    } else if is_proc_pointer_component {
                        (layout.proc_ptr_component(), layout.ptr_align)
                    } else if is_pointer && !declared_array && !matches!(ti, TypeInfo::Class(_)) {
                        (layout.ptr_bytes, layout.ptr_align) // scalar POINTER component: a pointer slot, not a descriptor
                    } else if is_allocatable || is_pointer {
                        layout.array_descriptor() // allocatable/pointer array component
                    } else if let TypeInfo::Derived(ref dname) = ti {
                        owner_scope
                            .and_then(|scope| registry.get_for_scope(scope, dname))
                            .or_else(|| registry.get(dname))
                            .map(|l| (l.size, l.align))
                            .unwrap_or((layout.ptr_bytes, layout.ptr_align))
                    } else {
                        size_of_type(&ti, layout)
                    };
                // Pad to alignment.
                let padding = (elem_align - (offset % elem_align)) % elem_align;
                offset += padding;
                max_align = max_align.max(elem_align);
                let elem_count = if dims.is_empty() {
                    1usize
                } else {
                    dims.iter()
                        .map(|(_, extent)| (*extent).max(0) as usize)
                        .product::<usize>()
                };
                let field_size = elem_size.saturating_mul(elem_count.max(1));
                // Procedure pointer components are parsed as
                // TypeSpec::Type(<iface>) with `pointer` and `external`
                // attrs, with the initial `=> target_proc` association
                // landing in `ptr_init`.  These are 8-byte slots, not
                // descriptors, and the initialization writes the
                // function address — not a const integer or character.
                let default_init = if is_proc_pointer_component {
                    if let Some(init_expr) = entity.ptr_init.as_ref() {
                        if let crate::ast::expr::Expr::Name { name } = &init_expr.node {
                            // Store the bare source-level target name.
                            // A post-pass in `sema::resolve` (after
                            // USE-rename associations are installed)
                            // rewrites this to the link-time symbol
                            // following the procedure's actual origin
                            // module — required because a name like
                            // `default_hasher` may be a `use ..., only:`
                            // alias for `fnv_1_hasher` defined in a
                            // sibling module.
                            Some(FieldDefaultInit::ProcedurePointer(name.clone()))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else if dims.is_empty() && !is_allocatable && !is_pointer {
                    entity.init.as_ref().and_then(|init| {
                        eval_const_field_default_init_for_layout(
                            &ti,
                            init,
                            registry,
                            const_params,
                            const_char_params,
                            const_derived_field_inits,
                        )
                    })
                } else {
                    None
                };

                fields.push(FieldLayout {
                    name: entity.name.clone(),
                    access: component_access,
                    owner_module: host_module.map(str::to_string),
                    offset,
                    size: field_size,
                    dims,
                    declared_array,
                    type_info: ti.clone(),
                    allocatable: is_allocatable,
                    pointer: is_pointer,
                    deferred_char,
                    target: is_target,
                    procedure_pointer: is_proc_pointer_component,
                    procedure_pointer_nopass,
                    default_init,
                });
                offset += field_size;
            }
        }
    }

    // Pad total size to struct alignment.
    if max_align > 0 {
        let padding = (max_align - (offset % max_align)) % max_align;
        offset += padding;
    }

    fn lowered_bound_proc_target(host_module: Option<&str>, target: &str) -> String {
        // Body emission via `module_procedure_symbol_name` lowercases
        // only the module name and preserves the procedure's source
        // case (see `module_procedure_case_and_bind_label_survive_amod_
        // import`). The TBP target stored in the type layout drives
        // the call-site `bl <target>` and must match exactly. Without
        // matching, `procedure :: pid => process_get_ID` defines
        // `_afs_modproc_<mod>_process_get_ID` while every TBP dispatch
        // looked up `_afs_modproc_<mod>_process_get_id` — caught at
        // stdlib `example_process_5` link.
        if let Some(module_name) = host_module {
            format!("afs_modproc_{}_{}", module_name.to_lowercase(), target)
        } else {
            target.to_string()
        }
    }

    fn upsert_bound_proc(bound_procs: &mut Vec<BoundProc>, proc: BoundProc) {
        if let Some(existing) = bound_procs.iter_mut().find(|bp| {
            bp.method_name.eq_ignore_ascii_case(&proc.method_name)
                && bp.abi_name.eq_ignore_ascii_case(&proc.abi_name)
        }) {
            *existing = proc;
        } else {
            bound_procs.push(proc);
        }
    }

    // Inherit parent's bindings, then let the local type override by method name.
    let mut bound_procs = parent_layout
        .map(|parent| parent.bound_procs.clone())
        .unwrap_or_default();
    for tbp in type_bound_procs {
        if tbp.is_generic {
            continue;
        }
        let target = tbp.binding.as_deref().unwrap_or(&tbp.name);
        let nopass = tbp.attrs.iter().any(|a| a.eq_ignore_ascii_case("nopass"));
        let existing_abi = bound_procs
            .iter()
            .find(|bp| bp.method_name.eq_ignore_ascii_case(&tbp.name))
            .map(|bp| bp.abi_name.clone());
        let target_name = lowered_bound_proc_target(host_module, target);
        let abi_name = tbp
            .interface
            .as_ref()
            .map(|iface| iface.to_lowercase())
            .or(existing_abi)
            .unwrap_or_else(|| target.to_lowercase());
        let proc = BoundProc {
            method_name: tbp.name.clone(),
            target_name,
            abi_name,
            nopass,
        };
        upsert_bound_proc(&mut bound_procs, proc);
        if let Some(override_proc) = bound_procs
            .iter()
            .find(|bp| bp.method_name.eq_ignore_ascii_case(&tbp.name))
            .cloned()
        {
            for inherited_alias in &mut bound_procs {
                if inherited_alias
                    .method_name
                    .eq_ignore_ascii_case(&override_proc.method_name)
                {
                    continue;
                }
                if inherited_alias
                    .abi_name
                    .eq_ignore_ascii_case(&override_proc.abi_name)
                {
                    inherited_alias.target_name = override_proc.target_name.clone();
                    inherited_alias.nopass = override_proc.nopass;
                }
            }
        }
    }

    for tbp in type_bound_procs {
        if !tbp.is_generic {
            continue;
        }
        for specific in &tbp.bindings {
            let alias = bound_procs
                .iter()
                .find(|bp| bp.method_name.eq_ignore_ascii_case(specific))
                .cloned()
                .unwrap_or_else(|| BoundProc {
                    method_name: tbp.name.clone(),
                    target_name: lowered_bound_proc_target(host_module, specific),
                    abi_name: specific.to_lowercase(),
                    nopass: false,
                });
            upsert_bound_proc(
                &mut bound_procs,
                BoundProc {
                    method_name: tbp.name.clone(),
                    target_name: alias.target_name,
                    abi_name: alias.abi_name,
                    nopass: alias.nopass,
                },
            );
        }
    }

    let final_procs: Vec<FinalProc> = final_proc_names
        .iter()
        .map(|name| {
            let name = if let Some(module_name) = host_module {
                format!(
                    "afs_modproc_{}_{}",
                    module_name.to_lowercase(),
                    name.to_lowercase()
                )
            } else {
                name.clone()
            };
            FinalProc {
                name,
                rank: 0,
                elemental: false,
            }
        })
        .collect();

    TypeLayout {
        name: type_name.to_string(),
        owner_module: host_module.map(str::to_string),
        owner_scope,
        owner_path: owner_path.map(str::to_string),
        size: offset,
        align: max_align,
        fields,
        bound_procs,
        final_procs,
        type_tag: 0, // assigned by registry after insertion
        parent: parent_layout.map(|p| registry.canonical_key_for_layout(p)),
        is_abstract,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_of_basic_types() {
        assert_eq!(
            size_of_type(
                &TypeInfo::Integer { kind: Some(4) },
                crate::target::TargetLayout::LP64
            ),
            (4, 4)
        );
        assert_eq!(
            size_of_type(
                &TypeInfo::Integer { kind: Some(8) },
                crate::target::TargetLayout::LP64
            ),
            (8, 8)
        );
        assert_eq!(
            size_of_type(
                &TypeInfo::Real { kind: Some(4) },
                crate::target::TargetLayout::LP64
            ),
            (4, 4)
        );
        assert_eq!(
            size_of_type(
                &TypeInfo::Real { kind: Some(8) },
                crate::target::TargetLayout::LP64
            ),
            (8, 8)
        );
        assert_eq!(
            size_of_type(
                &TypeInfo::Logical { kind: Some(4) },
                crate::target::TargetLayout::LP64
            ),
            (4, 4)
        );
        assert_eq!(
            size_of_type(
                &TypeInfo::Character {
                    len: Some(10),
                    kind: None
                },
                crate::target::TargetLayout::LP64
            ),
            (10, 1)
        );
        assert_eq!(
            size_of_type(
                &TypeInfo::Complex { kind: Some(4) },
                crate::target::TargetLayout::LP64
            ),
            (8, 4)
        );
        assert_eq!(
            size_of_type(
                &TypeInfo::Complex { kind: Some(8) },
                crate::target::TargetLayout::LP64
            ),
            (16, 8)
        );
    }

    #[test]
    fn layout_field_lookup() {
        let layout = TypeLayout {
            name: "point".into(),
            owner_module: None,
            owner_scope: None,
            owner_path: None,
            size: 8,
            align: 4,
            fields: vec![
                FieldLayout {
                    name: "x".into(),
                    access: Access::Public,
                    owner_module: None,
                    offset: 0,
                    size: 4,
                    dims: vec![],
                    declared_array: false,
                    type_info: TypeInfo::Real { kind: Some(4) },
                    allocatable: false,
                    pointer: false,
                    deferred_char: false,
                    target: false,
                    procedure_pointer: false,
                    procedure_pointer_nopass: false,
                    default_init: None,
                },
                FieldLayout {
                    name: "y".into(),
                    access: Access::Public,
                    owner_module: None,
                    offset: 4,
                    size: 4,
                    dims: vec![],
                    declared_array: false,
                    type_info: TypeInfo::Real { kind: Some(4) },
                    allocatable: false,
                    pointer: false,
                    deferred_char: false,
                    target: false,
                    procedure_pointer: false,
                    procedure_pointer_nopass: false,
                    default_init: None,
                },
            ],
            bound_procs: vec![],
            final_procs: vec![],
            type_tag: 0,
            parent: None,
            is_abstract: false,
        };
        assert_eq!(layout.field("x").unwrap().offset, 0);
        assert_eq!(layout.field("y").unwrap().offset, 4);
        assert_eq!(layout.field("X").unwrap().offset, 0); // case insensitive
        assert!(layout.field("z").is_none());
    }

    #[test]
    fn layout_mixed_types_padding() {
        // type :: mixed
        //   integer(1) :: a   ! 1 byte, offset 0
        //   real(8)    :: b   ! 8 bytes, offset 8 (padded to 8-byte alignment)
        //   integer(4) :: c   ! 4 bytes, offset 16
        // end type            ! total: 24 bytes (padded to 8-byte alignment)
        let layout = TypeLayout {
            name: "mixed".into(),
            owner_module: None,
            owner_scope: None,
            owner_path: None,
            size: 24,
            align: 8,
            fields: vec![
                FieldLayout {
                    name: "a".into(),
                    access: Access::Public,
                    owner_module: None,
                    offset: 0,
                    size: 1,
                    dims: vec![],
                    declared_array: false,
                    type_info: TypeInfo::Integer { kind: Some(1) },
                    allocatable: false,
                    pointer: false,
                    deferred_char: false,
                    target: false,
                    procedure_pointer: false,
                    procedure_pointer_nopass: false,
                    default_init: None,
                },
                FieldLayout {
                    name: "b".into(),
                    access: Access::Public,
                    owner_module: None,
                    offset: 8,
                    size: 8,
                    dims: vec![],
                    declared_array: false,
                    type_info: TypeInfo::Real { kind: Some(8) },
                    allocatable: false,
                    pointer: false,
                    deferred_char: false,
                    target: false,
                    procedure_pointer: false,
                    procedure_pointer_nopass: false,
                    default_init: None,
                },
                FieldLayout {
                    name: "c".into(),
                    access: Access::Public,
                    owner_module: None,
                    offset: 16,
                    size: 4,
                    dims: vec![],
                    declared_array: false,
                    type_info: TypeInfo::Integer { kind: Some(4) },
                    allocatable: false,
                    pointer: false,
                    deferred_char: false,
                    target: false,
                    procedure_pointer: false,
                    procedure_pointer_nopass: false,
                    default_init: None,
                },
            ],
            bound_procs: vec![],
            final_procs: vec![],
            type_tag: 0,
            parent: None,
            is_abstract: false,
        };
        // Verify padding: a(1) + 7 pad + b(8) + c(4) + 4 pad = 24
        assert_eq!(layout.size, 24);
        assert_eq!(layout.align, 8);
        assert_eq!(layout.field("a").unwrap().offset, 0);
        assert_eq!(layout.field("b").unwrap().offset, 8);
        assert_eq!(layout.field("c").unwrap().offset, 16);
    }

    #[test]
    fn registry_lookup() {
        let mut reg = TypeLayoutRegistry::new();
        reg.insert(TypeLayout {
            name: "MyType".into(),
            owner_module: None,
            owner_scope: None,
            owner_path: None,
            size: 16,
            align: 8,
            fields: vec![],
            bound_procs: vec![],
            final_procs: vec![],
            type_tag: 0,
            parent: None,
            is_abstract: false,
        });
        assert!(reg.get("mytype").is_some()); // case insensitive
        assert!(reg.get("MYTYPE").is_some());
        assert!(reg.get("other").is_none());
    }

    #[test]
    fn registry_assigns_distinct_stable_tags_to_module_siblings() {
        let alpha = TypeLayout {
            name: "child_t".into(),
            owner_module: Some("alpha_m".into()),
            owner_scope: None,
            owner_path: Some("alpha_m".into()),
            size: 8,
            align: 8,
            fields: vec![],
            bound_procs: vec![],
            final_procs: vec![],
            type_tag: 0,
            parent: Some("base_t".into()),
            is_abstract: false,
        };
        let beta = TypeLayout {
            name: "child_t".into(),
            owner_module: Some("beta_m".into()),
            owner_scope: None,
            owner_path: Some("beta_m".into()),
            size: 8,
            align: 8,
            fields: vec![],
            bound_procs: vec![],
            final_procs: vec![],
            type_tag: 0,
            parent: Some("base_t".into()),
            is_abstract: false,
        };

        assert_ne!(stable_type_tag(&alpha), stable_type_tag(&beta));
    }

    #[test]
    fn registry_resolves_same_named_layouts_through_scoped_renames() {
        let mut reg = TypeLayoutRegistry::new();
        reg.register_scope(1, Some(0), "alpha_m".into());
        reg.register_scope(2, Some(0), "beta_m".into());
        reg.register_scope(3, Some(0), "program:p".into());
        reg.insert(TypeLayout {
            name: "item_t".into(),
            owner_module: Some("alpha_m".into()),
            owner_scope: Some(1),
            owner_path: Some("alpha_m".into()),
            size: 4,
            align: 4,
            fields: vec![],
            bound_procs: vec![],
            final_procs: vec![],
            type_tag: 0,
            parent: None,
            is_abstract: false,
        });
        reg.insert(TypeLayout {
            name: "item_t".into(),
            owner_module: Some("beta_m".into()),
            owner_scope: Some(2),
            owner_path: Some("beta_m".into()),
            size: 8,
            align: 8,
            fields: vec![],
            bound_procs: vec![],
            final_procs: vec![],
            type_tag: 0,
            parent: None,
            is_abstract: false,
        });
        reg.bind_layout(3, "alpha_item", 1, "item_t");
        reg.bind_layout(3, "beta_item", 2, "item_t");
        reg.rebuild_alias_index();

        assert!(reg.get("item_t").is_none());
        assert_eq!(reg.get_for_scope(3, "alpha_item").unwrap().size, 4);
        assert_eq!(reg.get_for_scope(3, "beta_item").unwrap().size, 8);
        assert_eq!(
            reg.get("alpha_item").unwrap().owner_module.as_deref(),
            Some("alpha_m")
        );
        assert_eq!(
            reg.get("beta_item").unwrap().owner_module.as_deref(),
            Some("beta_m")
        );
    }

    /// Helper: build a component declaration for testing compute_layout.
    fn make_component(name: &str, ts: crate::ast::decl::TypeSpec) -> crate::ast::decl::SpannedDecl {
        use crate::ast::decl::*;
        use crate::ast::Spanned;
        let pos = crate::lexer::Position { line: 0, col: 0 };
        let span = crate::lexer::Span {
            start: pos,
            end: pos,
            file_id: 0,
        };
        Spanned::new(
            Decl::TypeDecl {
                type_spec: ts,
                attrs: vec![],
                entities: vec![EntityDecl {
                    name: name.to_string(),
                    array_spec: None,
                    char_len: None,
                    init: None,
                    ptr_init: None,
                }],
            },
            span,
        )
    }

    fn empty_params() -> std::collections::HashMap<String, i64> {
        std::collections::HashMap::new()
    }

    fn make_component_with_attrs(
        name: &str,
        ts: crate::ast::decl::TypeSpec,
        attrs: Vec<crate::ast::decl::Attribute>,
    ) -> crate::ast::decl::SpannedDecl {
        use crate::ast::decl::*;
        use crate::ast::Spanned;
        let pos = crate::lexer::Position { line: 0, col: 0 };
        let span = crate::lexer::Span {
            start: pos,
            end: pos,
            file_id: 0,
        };
        Spanned::new(
            Decl::TypeDecl {
                type_spec: ts,
                attrs,
                entities: vec![EntityDecl {
                    name: name.to_string(),
                    array_spec: None,
                    char_len: None,
                    init: None,
                    ptr_init: None,
                }],
            },
            span,
        )
    }

    fn make_component_with_init(
        name: &str,
        ts: crate::ast::decl::TypeSpec,
        init: crate::ast::expr::Expr,
    ) -> crate::ast::decl::SpannedDecl {
        use crate::ast::decl::*;
        use crate::ast::Spanned;
        let pos = crate::lexer::Position { line: 0, col: 0 };
        let span = crate::lexer::Span {
            start: pos,
            end: pos,
            file_id: 0,
        };
        Spanned::new(
            Decl::TypeDecl {
                type_spec: ts,
                attrs: vec![],
                entities: vec![EntityDecl {
                    name: name.to_string(),
                    array_spec: None,
                    char_len: None,
                    init: Some(Spanned::new(init, span)),
                    ptr_init: None,
                }],
            },
            span,
        )
    }

    #[test]
    fn compute_layout_simple_struct() {
        // type :: pair; integer :: x; real :: y; end type
        let reg = TypeLayoutRegistry::new();
        let components = vec![
            make_component("x", crate::ast::decl::TypeSpec::Integer(None)),
            make_component("y", crate::ast::decl::TypeSpec::Real(None)),
        ];
        let layout = compute_layout(
            "pair",
            None,
            &[],
            &[],
            &components,
            None,
            &reg,
            &empty_params(),
            crate::target::TargetLayout::LP64,
        );
        assert_eq!(layout.name, "pair");
        assert_eq!(layout.size, 8); // 4 + 4, no padding needed
        assert_eq!(layout.align, 4);
        assert_eq!(layout.fields.len(), 2);
        assert_eq!(layout.field("x").unwrap().offset, 0);
        assert_eq!(layout.field("y").unwrap().offset, 4);
    }

    #[test]
    fn compute_layout_with_padding() {
        // type :: padded; integer(1) :: a; real(8) :: b; end type
        // a(1) + 7pad + b(8) = 16
        let reg = TypeLayoutRegistry::new();
        let components = vec![
            make_component(
                "a",
                crate::ast::decl::TypeSpec::Integer(Some(crate::ast::decl::KindSelector::Expr(
                    crate::ast::Spanned::new(
                        crate::ast::expr::Expr::IntegerLiteral {
                            text: "1".into(),
                            kind: None,
                        },
                        crate::lexer::Span {
                            start: crate::lexer::Position { line: 0, col: 0 },
                            end: crate::lexer::Position { line: 0, col: 0 },
                            file_id: 0,
                        },
                    ),
                ))),
            ),
            make_component("b", crate::ast::decl::TypeSpec::DoublePrecision),
        ];
        let layout = compute_layout(
            "padded",
            None,
            &[],
            &[],
            &components,
            None,
            &reg,
            &empty_params(),
            crate::target::TargetLayout::LP64,
        );
        assert_eq!(layout.field("a").unwrap().offset, 0);
        assert_eq!(layout.field("a").unwrap().size, 1);
        assert_eq!(layout.field("b").unwrap().offset, 8); // padded to 8-byte alignment
        assert_eq!(layout.field("b").unwrap().size, 8);
        assert_eq!(layout.size, 16);
        assert_eq!(layout.align, 8);
    }

    #[test]
    fn compute_layout_with_extends() {
        // type :: base; integer :: x; end type
        // type, extends(base) :: child; real :: y; end type
        let mut reg = TypeLayoutRegistry::new();
        let base_comps = vec![make_component(
            "x",
            crate::ast::decl::TypeSpec::Integer(None),
        )];
        let base_layout = compute_layout(
            "base",
            None,
            &[],
            &[],
            &base_comps,
            None,
            &reg,
            &empty_params(),
            crate::target::TargetLayout::LP64,
        );
        assert_eq!(base_layout.size, 4);
        reg.insert(base_layout.clone());

        let child_comps = vec![make_component("y", crate::ast::decl::TypeSpec::Real(None))];
        let child_layout = compute_layout(
            "child",
            None,
            &[],
            &[],
            &child_comps,
            Some(&base_layout),
            &reg,
            &empty_params(),
            crate::target::TargetLayout::LP64,
        );
        assert_eq!(child_layout.fields.len(), 2); // x + y
        assert_eq!(child_layout.field("x").unwrap().offset, 0); // inherited
        assert_eq!(child_layout.field("y").unwrap().offset, 4); // appended
        assert_eq!(child_layout.size, 8);

        let explicit_parent = TypeInfo::Derived("base".into());
        assert!(structure_constructor_uses_positional_parent(
            &child_layout,
            &reg,
            Some(&explicit_parent),
        ));
        let positional_parent =
            structure_constructor_field(&child_layout, &reg, None, 0, true).unwrap();
        assert_eq!(positional_parent.name, "base");
        assert_eq!(
            positional_parent.type_info,
            TypeInfo::Derived("base".into())
        );
        assert_eq!(positional_parent.offset, 0);
        assert_eq!(positional_parent.size, 4);
        assert_eq!(
            structure_constructor_field(&child_layout, &reg, None, 1, true)
                .unwrap()
                .name,
            "y"
        );
        assert!(!structure_constructor_uses_positional_parent(
            &child_layout,
            &reg,
            Some(&TypeInfo::Integer { kind: None }),
        ));
        assert_eq!(
            structure_constructor_field(&child_layout, &reg, None, 0, false)
                .unwrap()
                .name,
            "x"
        );
        assert_eq!(
            structure_constructor_field(&child_layout, &reg, None, 1, false)
                .unwrap()
                .name,
            "y"
        );
        assert_eq!(
            structure_constructor_field(&child_layout, &reg, Some("base"), 0, false)
                .unwrap()
                .type_info,
            TypeInfo::Derived("base".into())
        );
    }

    #[test]
    fn compute_layout_scalar_derived_pointer_component_uses_pointer_slot() {
        let mut reg = TypeLayoutRegistry::new();
        reg.insert(TypeLayout {
            name: "node_t".into(),
            owner_module: None,
            owner_scope: None,
            owner_path: None,
            size: 16,
            align: 8,
            fields: vec![],
            bound_procs: vec![],
            final_procs: vec![],
            type_tag: 0,
            parent: None,
            is_abstract: false,
        });
        let components = vec![make_component_with_attrs(
            "left",
            crate::ast::decl::TypeSpec::Type("node_t".into()),
            vec![crate::ast::decl::Attribute::Pointer],
        )];

        let layout = compute_layout(
            "list_t",
            None,
            &[],
            &[],
            &components,
            None,
            &reg,
            &empty_params(),
            crate::target::TargetLayout::LP64,
        );
        let field = layout.field("left").expect("missing left field");

        assert_eq!(field.size, 8);
        assert_eq!(field.offset, 0);
        assert!(field.pointer);
        assert_eq!(layout.size, 8);
    }

    #[test]
    fn compute_layout_deferred_char_array_component_with_dimension_attr_uses_array_descriptor() {
        use crate::ast::decl::{ArraySpec, Attribute, CharSelector, LenSpec, TypeSpec};

        let components = vec![make_component_with_attrs(
            "lines",
            TypeSpec::Character(Some(CharSelector {
                len: Some(LenSpec::Colon),
                kind: None,
            })),
            vec![
                Attribute::Allocatable,
                Attribute::Dimension(vec![ArraySpec::Deferred]),
            ],
        )];

        let reg = TypeLayoutRegistry::new();
        let layout = compute_layout(
            "item_t",
            None,
            &[],
            &[],
            &components,
            None,
            &reg,
            &empty_params(),
            crate::target::TargetLayout::LP64,
        );
        let field = layout.field("lines").expect("missing lines field");

        assert_eq!(field.size, 392);
        assert_eq!(field.dims, vec![(1, 0)]);
        assert!(field.allocatable);
        assert!(field.declared_array);
        assert!(field.deferred_char);
        assert!(matches!(
            field.type_info,
            TypeInfo::Character {
                len: None,
                kind: None
            }
        ));
    }

    #[test]
    fn compute_layout_fixed_component_dimension_attr_preserves_extents() {
        use crate::ast::decl::{ArraySpec, Attribute, TypeSpec};
        use crate::ast::expr::Expr;
        use crate::ast::Spanned;

        let pos = crate::lexer::Position { line: 0, col: 0 };
        let span = crate::lexer::Span {
            start: pos,
            end: pos,
            file_id: 0,
        };
        let upper = Spanned::new(
            Expr::IntegerLiteral {
                text: "512".into(),
                kind: None,
            },
            span,
        );

        let mut reg = TypeLayoutRegistry::new();
        let token_layout = compute_layout(
            "token",
            None,
            &[],
            &[],
            &[make_component("tag", TypeSpec::Integer(None))],
            None,
            &reg,
            &empty_params(),
            crate::target::TargetLayout::LP64,
        );
        reg.insert(token_layout);

        let components = vec![make_component_with_attrs(
            "pattern",
            TypeSpec::Type("token".into()),
            vec![Attribute::Dimension(vec![ArraySpec::Explicit {
                lower: None,
                upper,
            }])],
        )];
        let layout = compute_layout(
            "holder",
            None,
            &[],
            &[],
            &components,
            None,
            &reg,
            &empty_params(),
            crate::target::TargetLayout::LP64,
        );
        let field = layout.field("pattern").expect("missing pattern field");

        assert_eq!(field.dims, vec![(1, 512)]);
        assert_eq!(field.size, 4 * 512);
        assert!(field.declared_array);
    }

    #[test]
    fn compute_layout_captures_scalar_default_initializers() {
        let reg = TypeLayoutRegistry::new();
        let components = vec![
            make_component_with_init(
                "depth",
                crate::ast::decl::TypeSpec::Integer(None),
                crate::ast::expr::Expr::IntegerLiteral {
                    text: "7".into(),
                    kind: None,
                },
            ),
            make_component_with_init(
                "enabled",
                crate::ast::decl::TypeSpec::Logical(None),
                crate::ast::expr::Expr::LogicalLiteral {
                    value: true,
                    kind: None,
                },
            ),
            make_component_with_init(
                "ratio",
                crate::ast::decl::TypeSpec::Real(None),
                crate::ast::expr::Expr::RealLiteral {
                    text: "1.5".into(),
                    kind: None,
                },
            ),
            make_component_with_init(
                "tag",
                crate::ast::decl::TypeSpec::Character(Some(crate::ast::decl::CharSelector {
                    len: Some(crate::ast::decl::LenSpec::Expr(crate::ast::Spanned::new(
                        crate::ast::expr::Expr::IntegerLiteral {
                            text: "4".into(),
                            kind: None,
                        },
                        crate::lexer::Span {
                            start: crate::lexer::Position { line: 0, col: 0 },
                            end: crate::lexer::Position { line: 0, col: 0 },
                            file_id: 0,
                        },
                    ))),
                    kind: None,
                })),
                crate::ast::expr::Expr::StringLiteral {
                    value: "".into(),
                    kind: None,
                },
            ),
        ];

        let layout = compute_layout(
            "state_t",
            None,
            &[],
            &[],
            &components,
            None,
            &reg,
            &empty_params(),
            crate::target::TargetLayout::LP64,
        );

        assert_eq!(
            layout
                .field("depth")
                .and_then(|field| field.default_init.clone()),
            Some(FieldDefaultInit::Integer(7))
        );
        assert_eq!(
            layout
                .field("enabled")
                .and_then(|field| field.default_init.clone()),
            Some(FieldDefaultInit::Logical(true))
        );
        assert_eq!(
            layout
                .field("ratio")
                .and_then(|field| field.default_init.clone()),
            Some(FieldDefaultInit::Real(1.5))
        );
        assert_eq!(
            layout
                .field("tag")
                .and_then(|field| field.default_init.clone()),
            Some(FieldDefaultInit::Character(String::new()))
        );
    }

    #[test]
    fn compute_layout_captures_derived_component_constructor_defaults() {
        use crate::ast::decl::TypeSpec;
        use crate::ast::expr::{Argument, Expr, SectionSubscript};
        use crate::ast::Spanned;

        let pos = crate::lexer::Position { line: 0, col: 0 };
        let span = crate::lexer::Span {
            start: pos,
            end: pos,
            file_id: 0,
        };

        let nested_components = vec![
            make_component_with_init(
                "style",
                TypeSpec::Integer(None),
                Expr::IntegerLiteral {
                    text: "-1".into(),
                    kind: Some("i1".into()),
                },
            ),
            make_component_with_init(
                "bg",
                TypeSpec::Integer(None),
                Expr::IntegerLiteral {
                    text: "-1".into(),
                    kind: Some("i1".into()),
                },
            ),
            make_component_with_init(
                "fg",
                TypeSpec::Integer(None),
                Expr::IntegerLiteral {
                    text: "-1".into(),
                    kind: Some("i1".into()),
                },
            ),
        ];
        let mut reg = TypeLayoutRegistry::new();
        reg.insert(compute_layout(
            "color_code",
            None,
            &[],
            &[],
            &nested_components,
            None,
            &reg,
            &empty_params(),
            crate::target::TargetLayout::LP64,
        ));

        let components = vec![
            make_component_with_init(
                "bold",
                TypeSpec::Type("color_code".into()),
                Expr::FunctionCall {
                    callee: Box::new(Spanned::new(
                        Expr::Name {
                            name: "color_code".into(),
                        },
                        span,
                    )),
                    args: vec![Argument {
                        keyword: Some("style".into()),
                        value: SectionSubscript::Element(Spanned::new(
                            Expr::IntegerLiteral {
                                text: "1".into(),
                                kind: Some("i1".into()),
                            },
                            span,
                        )),
                    }],
                },
            ),
            make_component_with_init(
                "blue",
                TypeSpec::Type("color_code".into()),
                Expr::FunctionCall {
                    callee: Box::new(Spanned::new(
                        Expr::Name {
                            name: "color_code".into(),
                        },
                        span,
                    )),
                    args: vec![Argument {
                        keyword: Some("fg".into()),
                        value: SectionSubscript::Element(Spanned::new(
                            Expr::IntegerLiteral {
                                text: "4".into(),
                                kind: Some("i1".into()),
                            },
                            span,
                        )),
                    }],
                },
            ),
        ];

        let layout = compute_layout(
            "color_output",
            None,
            &[],
            &[],
            &components,
            None,
            &reg,
            &empty_params(),
            crate::target::TargetLayout::LP64,
        );

        assert_eq!(
            layout
                .field("bold")
                .and_then(|field| field.default_init.clone()),
            Some(FieldDefaultInit::Derived(vec![(
                "style".into(),
                FieldDefaultInit::Integer(1),
            )]))
        );
        assert_eq!(
            layout
                .field("blue")
                .and_then(|field| field.default_init.clone()),
            Some(FieldDefaultInit::Derived(vec![(
                "fg".into(),
                FieldDefaultInit::Integer(4),
            )]))
        );
    }

    #[test]
    fn compute_layout_resolves_named_character_length_params() {
        use crate::ast::decl::{CharSelector, LenSpec, TypeSpec};
        use crate::ast::expr::Expr;
        use crate::ast::Spanned;

        let pos = crate::lexer::Position { line: 0, col: 0 };
        let span = crate::lexer::Span {
            start: pos,
            end: pos,
            file_id: 0,
        };
        let components = vec![make_component(
            "value",
            TypeSpec::Character(Some(CharSelector {
                len: Some(LenSpec::Expr(Spanned::new(
                    Expr::Name {
                        name: "MAX_TOKEN_LEN".into(),
                    },
                    span,
                ))),
                kind: None,
            })),
        )];
        let reg = TypeLayoutRegistry::new();
        let mut params = std::collections::HashMap::new();
        params.insert("max_token_len".into(), 8);

        let layout = compute_layout(
            "token_t",
            None,
            &[],
            &[],
            &components,
            None,
            &reg,
            &params,
            crate::target::TargetLayout::LP64,
        );
        let field = layout.field("value").expect("missing value field");

        assert_eq!(field.size, 8);
        assert!(matches!(
            field.type_info,
            TypeInfo::Character {
                len: Some(8),
                kind: None
            }
        ));
    }
}
