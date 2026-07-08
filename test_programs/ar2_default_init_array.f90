! CHECK: alloc n=7
! CHECK: mold n=2 7
! CHECK: func n=7
! CHECK: out n=7
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module ar2_default_init_array_mod
  implicit none

  type :: t
    integer :: n = 7
  end type t
contains
  subroutine reset(x)
    type(t), intent(out) :: x(3)

    print '(a,i0)', 'out n=', x(2)%n
  end subroutine reset

  function make_alloc() result(r)
    type(t), allocatable :: r(:)

    allocate(r(3))
  end function make_alloc
end module ar2_default_init_array_mod

program ar2_default_init_array
  use ar2_default_init_array_mod, only: make_alloc, reset, t
  implicit none

  type(t), allocatable :: da(:)
  type(t), allocatable :: molded(:)
  type(t), allocatable :: f(:)
  type(t) :: b(3)
  type(t) :: mold_src(2)

  allocate(da(3))
  print '(a,i0)', 'alloc n=', da(2)%n

  mold_src%n = 88
  allocate(molded, mold=mold_src)
  print '(a,i0,1x,i0)', 'mold n=', size(molded), molded(1)%n

  f = make_alloc()
  print '(a,i0)', 'func n=', f(2)%n

  b%n = 88
  call reset(b)
end program ar2_default_init_array
