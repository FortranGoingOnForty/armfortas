! XFAIL: derived-type function result assignment SIGSEGV — Task #486
! CHECK: 5
! audit31: assigning a derived-type function result to a variable
!          (c = add_t(a,b)) crashes with SIGSEGV at runtime.
! Accessing the same result via % chain (add_t(a,b)%x) succeeds but
! returns zero (body assignments apparently lost here — or default init value).
! Expected: prints '4 6'
program audit31_derived_fn_assign
  implicit none
  type :: t
    integer :: x = 0, y = 0
  end type
  type(t) :: a, b, c
  a%x = 1; a%y = 2
  b%x = 3; b%y = 4
  c = add_t(a, b)           ! <-- SIGSEGV here
  print *, c%x, c%y
contains
  function add_t(a, b) result(r)
    type(t), intent(in) :: a, b
    type(t) :: r
    r%x = a%x + b%x
    r%y = a%y + b%y
  end function
end program
