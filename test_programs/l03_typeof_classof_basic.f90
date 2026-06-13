! l03: F2023 TYPEOF/CLASSOF — declarations resolve to the referenced
! entity's declared type: intrinsic (integer, real(8)), derived (the
! TYPEOF(point) local gets real derived storage; its component stores
! must not be dropped), and CLASSOF as a polymorphic dummy carrying
! the caller-built class descriptor.
! FLAGS: --std=f2023
! CHECK: m 8
! CHECK: qx 6
! CHECK: obj 9
! CHECK: ok
program l03_typeof_classof_basic
  implicit none
  type :: point
    integer :: x
  end type
  integer :: n
  real(8) :: r
  typeof(n) :: m
  typeof(r) :: s
  type(point) :: p
  typeof(p) :: q

  n = 7
  m = n + 1
  s = 2.5d0
  print '(A,1X,I0)', 'm', m

  p%x = 3
  q%x = p%x * 2
  print '(A,1X,I0)', 'qx', q%x

  p%x = 9
  call show(p)
  print '(A)', 'ok'
contains
  subroutine show(obj)
    classof(p), intent(in) :: obj
    print '(A,1X,I0)', 'obj', obj%x
  end subroutine
end program l03_typeof_classof_basic
