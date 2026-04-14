! Host association: contained procedure reads a host-local scalar.
! Regression: proper closure passing (hidden trailing pointer param)
! preserves host storage semantics without promoting the scalar to
! a global, so the optimizer still sees it as a plain local alloca.
! CHECK: 42
! CHECK: 42
program host_local_scalar
  implicit none
  integer :: x
  x = 42
  call show()
  print *, x
contains
  subroutine show()
    print *, x
  end subroutine
end program
