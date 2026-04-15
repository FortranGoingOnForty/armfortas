! audit31 Finding 18: a multi-line CHARACTER literal continued with a
! trailing `&` and resumed on the next line whose first non-blank
! character is `!` produced "lexer error: unterminated string literal".
! The preprocessor's per-line macro expander walked into the open
! string on line N, exited at end-of-line, and on line N+1 — having
! lost the in-string state — treated the leading `!` (after the
! continuation marker) as the start of a Fortran comment and stripped
! the rest of the literal, including the closing quote. Sprint 31
! #470 fixed the single-line `&!` case; this is the continuation
! counterpart. Fix: have find_code_trailing_ampersand recognise a
! trailing `&` inside an unterminated string as a continuation so the
! preprocessor joins the lines BEFORE expansion runs. Task #499.
! CHECK: hello !world
program audit31_bang_amp_multi
  implicit none
  character(len=12) :: s
  s = 'hello &
       &!world'
  print *, trim(s)
end program
