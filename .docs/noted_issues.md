# Noted Issues

Minor issues accepted during audits that don't block forward progress. Fix if they surface in practice.

## Lexer (Sprint 5-6)

- **Hollerith with embedded single quotes**: `5HIT'SA` — the protect-to-quoted-string conversion creates a malformed string because the `'` inside the content isn't doubled. Fix: double any `'` characters inside Hollerith content before wrapping in quotes.

- **Blank lines between statements consumed by continuation scanning**: When a blank line follows a fixed-form statement, the continuation-scanning while-loop consumes it instead of emitting `FixedLine::Blank`. No functional impact on token stream.

- **`!` inline comments in fixed-form body**: The lexer accepts `!` as inline comment in columns 7-72. Not standard F77 (where `!` was only column-1 comment), but matches modern compiler behavior (gfortran, flang).

- **Column offsets after whitespace stripping**: In fixed-form, `pos` in the stripped string doesn't map to the original column. Span columns for body tokens may be inaccurate. Would need a position-mapping table to fix.

- **UTF-8 multi-byte characters**: Both lexers process bytes and cast with `as char`. Non-ASCII in comments or strings will produce garbled token text. Fortran identifiers are ASCII-only so this only affects string/comment content.

## Parser (Sprint 7)

- **`(/ /)` array constructor limitation**: Expressions inside the legacy `(/ ... /)` form cannot contain `*`, `/`, or lower-precedence infix operators because `/` is ambiguous with the closing `/)`. The bracket form `[...]` has no such limitation. The `(/ /)` form is legacy F90 — the bracket form is preferred and standard since F2003.

- **`AcValue::ImpliedDo` is 288 bytes**: Could be boxed to reduce enum size. Correctness unaffected.

## Parser — Declarations (Sprint 8)

- **AssumedRank `(..)` cannot lex**: The lexer errors on bare `.`. The parser handles `..` but the lexer never produces it. F2018 assumed-rank is extremely rare. Fix when lexer is revisited.

- **EnumDef has AST but no parser**: Fortran enums (`enum, bind(c)`) are rare. Add when needed.

- **Codimension attribute absent**: Coarray Fortran is exotic. Not blocking.

- **Standalone `AttributeStmt`** (`allocatable :: x`): AST exists, no parser. Add when needed for F77 code.

- **`parse_derived_type_def` not wired into dispatcher**: Correct by inspection but not callable until Sprint 10 adds statement-level parsing.

## Semantic Analysis — Symbol Tables (Sprint 12)

- **USE-without-ONLY renames create duplicate associations**: `USE mod, a => b` imports `b` under both names. The rename should replace, not add alongside. Fix when ambiguity checking lands.

- **Module scope re-entry mutates parent pointer**: Fragile but correct for current top-level-only modules. Add `enter_scope(id)` method for cleaner navigation.

- **`type_spec_to_info` discards kind selectors**: `integer(8)` becomes `Integer { kind: None }`. Kind info will be needed in Sprint 13 (type checking).

- **Missing: Submodule resolution, named interface registration, standalone PARAMETER/COMMON/AttributeStmt processing, ambiguous USE detection, function return_type propagation**: All incremental — architecture supports adding them.

- **Default access not applied to CONTAINS subprograms**: Subroutines/functions in modules use `Access::Default` instead of inheriting the module's `PRIVATE` default.

## Parser — Advanced Statements (Sprint 11)

- **FORMAT mini-language**: FORMAT strings are stored as expressions, not parsed into edit descriptors. The full FORMAT parser (I/F/E/ES/EN/G/D/A/L/B/O/Z + control descriptors + repeat groups) is deferred to the runtime I/O sprint (Sprint 24-25).

- **Coarray statements**: SYNC ALL/IMAGES/MEMORY, EVENT POST/WAIT, LOCK/UNLOCK, FAIL IMAGE, CHANGE TEAM — all deferred to coarray support sprint. Exotic features.

- **I/O keyword case preservation**: Keywords in IoControl store original case (`STAT` not `stat`). Downstream must use `eq_ignore_ascii_case` for matching.

- **`consume_end` eats any trailing identifier**: After `end do`, any following identifier is consumed as a potential construct name without newline boundary check. Safe in practice due to `skip_newlines()` in callers, but fragile.

- **DATA statement `/` delimiter conflict**: Same as `(/ /)` — expressions inside DATA `/ /` delimiters cannot contain `*` or `/` operators. Parsed at elevated binding power.

## Parser — Subprograms & Modules (Sprint 10)

- **ENTRY statement**: Legacy feature, not implemented. Extremely rare in modern code.

- **SeparateModuleProcedure**: F2008 feature, not implemented. Add when needed.

- **Statement functions**: `f(x) = x**2 + 2*x + 1` — ambiguous with array assignment. Deferred to semantic analysis which has the context to disambiguate.

- **Submodule discards imports/implicit**: `parse_submodule` ignores the `imports` and `implicit` from `parse_unit_body`. Minor gap — submodules rarely use IMPORT.

## Parser — Control Flow (Sprint 9)

- **SELECT TYPE (F2003)**: AST node exists in spec but not implemented. Belongs in Sprint 28 (OOP/derived types). Requires type guards and associate-name semantics.

- **CRITICAL construct (F2008 coarray)**: Not implemented. Coarray Fortran is exotic — defer until coarray support is considered.

- **Assigned GOTO**: `ASSIGN 10 TO L` / `GO TO L` — deprecated since F95, removed in F2018. Not implemented. Extremely rare in modern code.

- **LocalitySpec for DO CONCURRENT**: F2018 `LOCAL`, `LOCAL_INIT`, `SHARED`, `DEFAULT(NONE)`, `REDUCE` clauses. Not implemented. Few real codes use this yet.

- **Labeled DO (F77 style)**: `DO 10 I=1,10 ... 10 CONTINUE` — parser has no label-on-DO awareness. Labels on CONTINUE are always None. Resolution could be a semantic pass.

- **Construct name validation**: Names at `end do outer` are consumed but not validated against the opening name. A mismatch is silently accepted. Should be a semantic check.

- **`quiet` field on STOP/ERROR STOP**: Always `false`. The `QUIET=` specifier from modern Fortran is never parsed.

## Preprocessor (Sprint 4)

- **`expand_condition_macros` and `expand_macros_inner` are separate codepaths**: The condition expander uses a 3-pass approach (defined pre-pass, recursive expand, undefined→0). Any future macro expansion bug fix must be applied to both paths.

- **`is_emitting()` iterates the full condition stack per line**: O(n) in nesting depth. Could maintain a running counter instead. Not a concern in practice.

## Semantic Analysis — Type System (Sprint 13)

- **Convert node insertion deferred**: `needs_conversion()` detects implicit type mismatches but does not insert `Convert` nodes into the AST. Actual tree rewriting requires a typed IR or annotation pass. The detection infrastructure is complete; insertion belongs in the IR lowering phase.

- **Component access type resolution**: `expr_type` returns `Unknown` for `x%field` expressions. Resolving component types requires derived type definition lookup, which needs the type table populated during resolve. Add when derived type field access is needed.

- **Typed array constructor**: `[integer :: 1, 2, 3]` — the `type_spec` string is not resolved to a FortranType. Array constructors without a type_spec use the first element's type. Resolve type_spec when typed array constructors appear in practice.

- **ComplexLiteral kind detection**: Complex literals always return `complex(4)`. The actual kind should be determined from the component literals (real/imag). Fix when complex kind matters.

- **BOZ literal type context**: BOZ literals (`B'1010'`) always return `integer(4)`. Per standard, BOZ type depends on context (e.g., `real :: x = Z'3F800000'` → real). Fix when BOZ appears in typed contexts.

- **`real`/`dble` intrinsic ignores KIND argument**: `real(x, kind=8)` returns `real(4)` instead of `real(8)`. The intrinsic table doesn't examine the second argument for kind. Fix when kind-specific intrinsic calls appear.

- **Generic resolution uses exact type matching**: `resolve_generic` requires exact type equality between actual and dummy arguments. The standard allows TKR (type-kind-rank) matching with some flexibility. Sufficient for most generic interfaces.
