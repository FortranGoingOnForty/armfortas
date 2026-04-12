! Substring designator on scalar character variables.
!
! Previously the parser's FunctionCall { callee: Name(s), args:
! [Range(..)] } node for `s(1:n)` fell through to the general
! expression path in both lower_string_expr and the print/write
! `is_char` detector, emitting a call to an external function
! `_s` that failed at link time.
!
! Covers: literal bounds, variable upper bound, default lower,
! default upper, both defaults, and assignment to a character
! destination.
program char_substring
  implicit none
  character(len=10) :: s, t
  integer :: n

  s = "helloworld"
  n = 5

  ! Print paths.
  print *, s(1:5)
  print *, s(1:n)
  print *, s(6:10)
  print *, s(:5)
  print *, s(6:)
  print *, s(:)

  ! Assignment path — fixed-length destination.
  t = s(1:5)
  print *, t

  ! Empty substring (standard clamp to zero length).
  print *, "[", s(5:4), "]"
end program char_substring
! CHECK: hello
! CHECK: hello
! CHECK: world
! CHECK: hello
! CHECK: world
! CHECK: helloworld
! CHECK: hello
! CHECK: [ ]
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
