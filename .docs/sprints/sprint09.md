# Sprint 9: Parser — Control Flow

## Prerequisites
Sprint 7 (expressions), Sprint 8 (declarations)

## Goals
Parse all Fortran control flow constructs. Fortran has a rich set of control structures spanning five decades of language evolution — from F77's arithmetic IF and computed GOTO to F2008's DO CONCURRENT.

## Deliverables

### 1. AST Control Flow Nodes
```rust
enum Stmt {
    // Assignment
    Assignment { target: Expr, value: Expr },
    PointerAssignment { target: Expr, value: Expr },  // =>

    // IF construct
    IfConstruct {
        name: Option<String>,
        condition: Expr,
        then_body: Vec<Stmt>,
        else_ifs: Vec<(Expr, Vec<Stmt>)>,
        else_body: Option<Vec<Stmt>>,
    },
    // Single-line IF
    IfStmt { condition: Expr, action: Box<Stmt> },

    // DO loops
    DoLoop {
        name: Option<String>,
        var: Option<String>,
        start: Option<Expr>,
        end: Option<Expr>,
        step: Option<Expr>,
        body: Vec<Stmt>,
    },
    DoWhile {
        name: Option<String>,
        condition: Expr,
        body: Vec<Stmt>,
    },
    DoConcurrent {
        name: Option<String>,
        controls: Vec<ConcurrentControl>,
        mask: Option<Expr>,
        locality: Vec<LocalitySpec>,
        body: Vec<Stmt>,
    },

    // SELECT CASE
    SelectCase {
        name: Option<String>,
        selector: Expr,
        cases: Vec<CaseBlock>,
    },

    // SELECT TYPE (F2003)
    SelectType {
        name: Option<String>,
        selector: Expr,
        assoc_name: Option<String>,
        type_guards: Vec<TypeGuard>,
    },

    // FORALL / WHERE
    ForallConstruct { name: Option<String>, specs: Vec<ForallSpec>, mask: Option<Expr>, body: Vec<Stmt> },
    ForallStmt { specs: Vec<ForallSpec>, mask: Option<Expr>, stmt: Box<Stmt> },
    WhereConstruct { name: Option<String>, mask: Expr, body: Vec<Stmt>, elsewhere: Vec<(Option<Expr>, Vec<Stmt>)> },
    WhereStmt { mask: Expr, stmt: Box<Stmt> },

    // BLOCK (F2008)
    Block { name: Option<String>, body: Vec<Stmt> },

    // ASSOCIATE (F2003)
    Associate { name: Option<String>, assocs: Vec<(String, Expr)>, body: Vec<Stmt> },

    // Branch/transfer
    Exit { name: Option<String> },
    Cycle { name: Option<String> },
    Stop { code: Option<Expr>, quiet: bool },
    ErrorStop { code: Option<Expr>, quiet: bool },
    Return { value: Option<Expr> },
    Goto { label: u64 },
    ComputedGoto { labels: Vec<u64>, selector: Expr },
    ArithmeticIf { expr: Expr, neg: u64, zero: u64, pos: u64 },

    // Labels
    Continue { label: Option<u64> },  // labeled CONTINUE (DO loop termination)

    // CRITICAL (F2008, coarray)
    Critical { name: Option<String>, body: Vec<Stmt> },
}
```

### 2. IF Construct
```fortran
if (x > 0) then
    call positive(x)
else if (x < 0) then
    call negative(x)
else if (x == 0) then
    call zero_handler()
else
    call unknown()
end if
```

Also single-line:
```fortran
if (x > 0) y = sqrt(x)
if (done) stop
```

Named constructs:
```fortran
check: if (x > 0) then
    y = sqrt(x)
end if check
```

### 3. DO Loops
```fortran
! Counted DO
do i = 1, 100
    ...
end do

! With step
do i = 100, 1, -1
    ...
end do

! DO WHILE
do while (x > epsilon)
    x = x / 2.0
end do

! Infinite DO
do
    if (converged) exit
end do

! DO CONCURRENT (F2008)
do concurrent (i = 1:n, j = 1:m, i /= j)
    a(i,j) = b(i,j) + c(i,j)
end do

! Named DO
outer: do i = 1, n
    inner: do j = 1, m
        if (a(i,j) < 0) exit outer
    end do inner
end do outer

! Labeled DO (F77 style)
      DO 10 I = 1, 10
         X = X + A(I)
   10 CONTINUE
```

### 4. SELECT CASE
```fortran
select case (grade)
case ('A', 'B')
    print *, 'Good'
case ('C')
    print *, 'Average'
case ('D':'F')
    print *, 'Poor'
case default
    print *, 'Unknown'
end select
```

Case selectors: single values, ranges (`low:high`), and default. Values can be integer, character, or logical.

### 5. Legacy Control Flow
Must parse even if we don't love it:
```fortran
! Arithmetic IF (F77, deprecated)
      IF (X) 10, 20, 30

! Computed GOTO (F77, deprecated)
      GO TO (10, 20, 30), I

! Assigned GOTO (even more deprecated, but exists)
      ASSIGN 10 TO L
      GO TO L

! Plain GOTO
      GO TO 100
```

### 6. WHERE and FORALL
```fortran
where (a > 0)
    b = sqrt(a)
elsewhere
    b = 0.0
end where

forall (i = 1:n, j = 1:n, i /= j)
    a(i,j) = 1.0 / real(i - j)
end forall
```

## Testing Strategy

### Structure Tests
For each construct, parse and verify AST structure:
- Correct nesting of if/else-if/else
- Correct capture of DO loop bounds
- Case selector ranges parsed correctly
- Named constructs link correctly (name at `do` matches name at `end do`)

### Nesting Tests
Deeply nested constructs:
```fortran
if (...) then
    do i = 1, n
        select case (x(i))
        case (1)
            do while (cond)
                if (...) exit
            end do
        end select
    end do
end if
```

### Label Handling
- Labeled DO loops with CONTINUE
- GOTO targets
- Arithmetic IF targets
- Verify labels are collected and accessible

### Error Recovery
- Missing END DO / END IF
- Mismatched construct names
- Missing THEN after IF condition

### fortsh Control Flow
Parse all control flow from fortsh source. fortsh is heavy on if/else and do loops — good real-world coverage.

## Definition of Done
- All IF forms parse (construct, single-line, named)
- All DO forms parse (counted, while, concurrent, infinite, named, labeled)
- SELECT CASE parses with all selector forms
- WHERE, FORALL, BLOCK, ASSOCIATE parse
- All legacy forms parse (arithmetic IF, computed GOTO, GOTO)
- EXIT/CYCLE with optional construct names parse
- STOP/ERROR STOP parse
- RETURN parse
- Nested constructs parse to correct depth
- Named constructs validated (name at end matches begin)
- fortsh control flow parses without error
- `cargo test` control flow parser tests pass
