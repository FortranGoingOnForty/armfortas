! `&!` inside a single-line string literal must be taken verbatim.
! Previously the lexer's string-continuation check treated `!` after
! `&` as a trailing-line comment, consumed through the closing quote
! and into the next line, and reported "unterminated string literal".
! CHECK: hello &!world
program t
  implicit none
  character(len=20) :: s
  s = 'hello &!world'
  print *, s
end program
