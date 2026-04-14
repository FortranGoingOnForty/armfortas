! XFAIL: character initializer silently blank — Task #484
! CHECK: [hello]
! audit31: character variable/parameter initializer is lost.
! Expected: '[hello]'
! Actual:   '[     ]' (blank, correct length)
! Literal strings passed directly to print DO work.
! Integer initializers work. Only character is broken.
program audit31_char_init
  implicit none
  character(len=5)           :: a = 'hello'
  character(len=5), parameter :: b = 'world'
  print *, '[', a, ']'
  print *, '[', b, ']'
end program
