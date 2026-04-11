# ARMFORTAS Sprint Index

51 sprints (36 original + 4 cleanup sprints + 5 completeness sprints + 6 deferred optimization sprints). Small bites, clear milestones, testable deliverables at every stage.

## Phase 1: Foundation (Sprints 0-3)
- [Sprint 0](sprint00.md) — Scaffolding & Learning
- [Sprint 1](sprint01.md) — afs-as: ARM64 Instruction Encoding
- [Sprint 2](sprint02.md) — afs-as: Assembly Text Parser
- [Sprint 3](sprint03.md) — afs-as: Mach-O Object Emission

## Phase 2: Preprocessor & Lexer (Sprints 4-6)
- [Sprint 4](sprint04.md) — Preprocessor
- [Sprint 5](sprint05.md) — Lexer: Free-Form
- [Sprint 6](sprint06.md) — Lexer: Fixed-Form & Multi-Mode

## Phase 3: Parser (Sprints 7-11)
- [Sprint 7](sprint07.md) — Parser: Expressions
- [Sprint 8](sprint08.md) — Parser: Declarations
- [Sprint 9](sprint09.md) — Parser: Control Flow
- [Sprint 10](sprint10.md) — Parser: Subprograms & Modules
- [Sprint 11](sprint11.md) — Parser: Advanced Features (I/O, Derived Types, Interfaces)

## Phase 4: Semantic Analysis (Sprints 12-14)
- [Sprint 12](sprint12.md) — Semantic Analysis: Symbol Tables & Scoping
- [Sprint 13](sprint13.md) — Semantic Analysis: Type System
- [Sprint 14](sprint14.md) — Semantic Analysis: Advanced Validation

## Phase 5: Intermediate Representation (Sprints 15-16)
- [Sprint 15](sprint15.md) — IR Design & Basic Construction
- [Sprint 16](sprint16.md) — IR: Complex Lowering (Arrays, Strings, Control Flow)

## Phase 6: Code Generation (Sprints 17-21)
- [Sprint 17](sprint17.md) — Codegen: Instruction Selection
- [Sprint 18](sprint18.md) — HELLO WORLD MILESTONE (End-to-End Pipeline)
- [Sprint 19](sprint19.md) — Codegen: Control Flow & Loops
- [Sprint 20](sprint20.md) — Codegen: Functions & Calling Convention (AAPCS64)
- [Sprint 21](sprint21.md) — Codegen: Register Allocation
- [Sprint 21.5](sprint21_5.md) — Deferred Item Cleanup

## Phase 7: Runtime Library (Sprints 22-26)
- [Sprint 22](sprint22.md) — Runtime: Memory Management & Descriptors
- [Sprint 23](sprint23.md) — Runtime: Strings (The Big One)
- [Sprint 24](sprint24.md) — Runtime: Basic I/O
- [Sprint 25](sprint25.md) — Runtime: Advanced I/O
- [Sprint 25.5](sprint25_5.md) — I/O Pipeline Completeness (Formatted I/O integration, Non-Advancing I/O)
- [Sprint 26](sprint26.md) — Runtime: Intrinsics (Math, Array, System)
- [Sprint 26.5](sprint26_5.md) — Complex Number Arithmetic

## Phase 8: Advanced Features (Sprints 27-29)
- [Sprint 27](sprint27.md) — iso_c_binding
- [Sprint 28](sprint28.md) — Derived Types & OOP
- [Sprint 28.5](sprint28_5.md) — Modern Fortran Feature Gaps (SELECT TYPE, AssumedRank, DO CONCURRENT locality, BOZ context, submodules)
- [Sprint 28.7](sprint28_7.md) — Array Expressions, Sections, WHERE, FORALL
- [Sprint 29](sprint29.md) — Optimization Passes (const fold/prop, DCE, CSE, LICM, strength reduction, DSE, FMA fusion, LDP/STP, loop unrolling, CSEL, tail call, load-store forwarding)
- [Sprint 29.5](sprint29_5.md) — Performance & Cleanup (value_type cache, AcValue boxing, preprocessor unification, I128)
- [Sprint 29.6](sprint29_6.md) — Loop Optimizations (fusion, fission, interchange, peeling, unswitching, NEON vectorization)
- [Sprint 29.7](sprint29_7.md) — Function Inlining (basic → threshold → aggressive → cross-module)
- [Sprint 29.8](sprint29_8.md) — Advanced IR Optimizations (GVN, SROA, alias analysis, load-store forwarding cross-block, bounds check elim)
- [Sprint 29.9](sprint29_9.md) — Fortran-Specific & Interprocedural Optimizations (no-alias exploitation, PURE/ELEMENTAL CSE, DO CONCURRENT, IPO)
- [Sprint 29.10](sprint29_10.md) — Sprint 29 Cleanup & Completion (finish 29.x leftovers one by one, with audit/hardening)
- [Sprint 29.11](sprint29_11.md) — Full Sprint 29 Audit (real-world reproducers, determinism, binary correctness, and closure)

## Phase 9: Integration (Sprints 30-32)
- [Sprint 30](sprint30.md) — Module System & Multi-File Compilation
- [Sprint 31](sprint31.md) — Multi-Standard Support & Fixed-Form Codegen
- [Sprint 31.5](sprint31_5.md) — Fixed-Form & F77 Legacy Completeness (Hollerith, labeled DO, assigned GOTO, column offsets, coarray stubs)
- [Sprint 32](sprint32.md) — CLI Driver & Build Integration

## Phase 10: The fortsh Milestone (Sprints 33-35)
- [Sprint 33](sprint33.md) — fortsh Compilation: Core Modules
- [Sprint 34](sprint34.md) — fortsh Compilation: Full Build
- [Sprint 35](sprint35.md) — Hardening & Polish

## Phase 11: Full Standard Compliance (Sprints 36-38)
- [Sprint 36](sprint36.md) — Standard Library Completeness (remaining intrinsics, IEEE module, iso_fortran_env, date/time, random)
- [Sprint 37](sprint37.md) — Error Handling & Diagnostics (ERRMSG=, IOMSG=, STAT= everywhere, runtime error recovery)
- [Sprint 38](sprint38.md) — Compliance Testing (run against standard test suites, fix what breaks)
