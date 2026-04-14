! Explicit IMPLICIT rules override the first-letter default: every
! undeclared name becomes INTEGER even if it starts with x-z.
! CHECK: 42 43
program implicit_override
  implicit integer (a-z)
  x = 42
  y = x + 1
  print *, x, y
end program
