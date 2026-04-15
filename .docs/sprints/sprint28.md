# Sprint 28: Derived Types & OOP

## Prerequisites
Sprint 22 (memory management), Sprint 13 (type system)

## Goals
Implement Fortran's object-oriented features: derived type codegen (memory layout, component access), type-bound procedures, inheritance, polymorphism, and finalization. fortsh uses derived types heavily (shell_state_t, command_t, pipeline_t, etc.) though not deep OOP. We still need full support for standard compliance.

## Deliverables

### 1. Derived Type Memory Layout
```fortran
type :: particle
    real(8) :: x, y, z          ! 8 bytes each
    real(8) :: mass              ! 8 bytes
    integer :: id                ! 4 bytes
    logical :: active            ! 4 bytes
end type                        ! total: 40 bytes (no padding needed due to alignment)
```

Layout rules (non-BIND(C)):
- Components laid out in declaration order
- Alignment: each component aligned to its natural alignment
- Struct alignment: maximum alignment of any component
- Compiler may insert padding between components

We use the same layout as a C struct for simplicity and interop, but this is a compiler choice (not mandated by the standard for non-BIND(C) types).

```rust
fn compute_type_layout(typ: &DerivedType) -> TypeLayout {
    let mut offset = 0;
    let mut max_align = 1;
    let mut fields = Vec::new();
    
    for component in &typ.components {
        let align = alignment_of(&component.type_);
        let size = size_of(&component.type_);
        max_align = max_align.max(align);
        
        // Pad to alignment
        offset = (offset + align - 1) & !(align - 1);
        fields.push(FieldLayout { offset, size });
        offset += size;
    }
    
    // Pad struct to alignment
    let struct_size = (offset + max_align - 1) & !(max_align - 1);
    TypeLayout { size: struct_size, align: max_align, fields }
}
```

### 2. Component Access Codegen
```fortran
p%x = 1.0
p%mass = 2.5
y = p%y
```

Lowers to field offset loads/stores:
```
%addr = getelementptr %p, field_offset(x)
store f64 1.0, %addr
```

### 3. Allocatable and Pointer Components
```fortran
type :: container
    real, allocatable :: data(:)
    type(node), pointer :: next => null()
end type
```

Allocatable components:
- Stored as array descriptors within the struct
- Automatic deallocation when parent is deallocated
- Deep copy on assignment (F2003)

Pointer components:
- Stored as raw pointers within the struct
- No automatic deallocation
- Pointer (shallow) copy on assignment

### 4. Type-Bound Procedures
```fortran
type :: shape
    real :: area_val
contains
    procedure :: area => compute_area
    procedure :: draw
    procedure, nopass :: create
end type

call my_shape%area()     ! passes my_shape as first arg (PASS)
call shape%create()      ! no passed object (NOPASS)
```

Codegen for type-bound procedure call:
1. Look up procedure in the type's vtable (for polymorphic) or directly (for non-polymorphic)
2. Pass the object as the first argument (unless NOPASS)
3. Call the procedure

### 5. Inheritance (Type Extension)
```fortran
type :: shape
    real :: x, y
end type

type, extends(shape) :: circle
    real :: radius
end type

type, extends(shape) :: rectangle
    real :: width, height
end type
```

Memory layout of `circle`: `{ x, y, radius }` — parent fields first, then extension fields.

Access parent components directly: `c%x` works for a `circle`.

### 6. Polymorphism (CLASS)
```fortran
class(shape), allocatable :: s
allocate(circle :: s)         ! s is a circle
s%x = 1.0                    ! access parent component
select type (s)
type is (circle)
    s%radius = 5.0            ! access extended component
type is (rectangle)
    s%width = 3.0
end select
```

Polymorphic variables carry a type tag at runtime:
```rust
#[repr(C)]
struct PolymorphicDescriptor {
    data: *mut u8,
    type_tag: TypeId,         // identifies actual dynamic type
    vtable: *const VTable,    // for type-bound procedure dispatch
}
```

### 7. Virtual Dispatch (Generic Type-Bound Procedures)
```fortran
type, abstract :: shape
contains
    procedure(area_iface), deferred :: area
end type

type, extends(shape) :: circle
    real :: radius
contains
    procedure :: area => circle_area
end type
```

Each type has a vtable. Polymorphic calls go through the vtable:
```asm
    ldr x8, [x0, #vtable_offset]     ; load vtable pointer
    ldr x9, [x8, #area_slot]         ; load function pointer
    blr x9                            ; indirect call
```

### 8. Finalization
```fortran
type :: managed_resource
    integer :: handle
contains
    final :: cleanup
end type

subroutine cleanup(self)
    type(managed_resource), intent(inout) :: self
    call close_handle(self%handle)
end subroutine
```

Final subroutines are called:
- When a variable goes out of scope
- When a variable is deallocated
- When a variable is the target of an intrinsic assignment (old value finalized)
- When a function result temporary is no longer needed

**This is where gfortran crashes on ARM64** (automatic finalization bug). Our implementation: insert explicit calls to the final subroutine at all required points during codegen.

### 9. Structure Constructors
```fortran
type(particle) :: p
p = particle(1.0, 2.0, 3.0, 10.0, 1, .true.)
p = particle(x=1.0, y=2.0, z=3.0, mass=10.0, id=1, active=.true.)
```

## Testing Strategy

### Layout Tests
Verify `sizeof` and member offsets match expected values. Cross-check with C structs using BIND(C) types.

### Component Access Tests
Create derived types, assign to components, read back, verify.

### Allocatable Component Tests
- Assign to allocatable component
- Copy a struct with allocatable component (must deep copy)
- Deallocate parent (must deallocate components)

### Inheritance Tests
- Create extended types, access parent and child components
- Pass child type to procedure expecting parent type

### Polymorphism Tests
- Allocate polymorphic variable with different actual types
- SELECT TYPE dispatches correctly
- Virtual dispatch calls correct procedure

### Finalization Tests
- Verify final subroutine called on scope exit
- Verify called on deallocation
- Verify called on assignment (old value)
- Verify NOT called on pointer assignment

### fortsh Derived Types
Parse and compile fortsh's derived types (shell_state_t, command_t, pipeline_t, etc.). Verify correct layout and component access.

## Definition of Done
- Derived type layout computed correctly
- Component access generates correct offset loads/stores
- Allocatable components with deep copy and auto-deallocation
- Type extension (inheritance) with correct memory layout
- Polymorphism with type tags and SELECT TYPE
- Virtual dispatch through vtables
- Finalization at all required points (no crashes!)
- Structure constructors work
- fortsh derived types compile correctly
- `cargo test` OOP tests pass
