! CHECK: self= T alpha
! CHECK: elem= T beta
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar8_selfassign_derived_scalar
  implicit none

  type :: entry_t
    character(len=:), allocatable :: path
  end type entry_t

  type :: guard_t
    type(entry_t), allocatable :: entries(:)
  end type guard_t

  type(guard_t) :: g
  type(guard_t) :: a(2)
  integer :: i
  integer :: j

  allocate(g%entries(1))
  g%entries(1)%path = 'alpha'

  g = g
  if (.not. allocated(g%entries)) error stop 1
  if (.not. allocated(g%entries(1)%path)) error stop 2
  if (g%entries(1)%path /= 'alpha') error stop 3
  print '(a,l1,1x,a)', 'self=', allocated(g%entries), g%entries(1)%path

  allocate(a(1)%entries(1))
  allocate(a(2)%entries(1))
  a(1)%entries(1)%path = 'beta'
  a(2)%entries(1)%path = 'gamma'
  i = 1
  j = 1

  a(i) = a(j)
  if (.not. allocated(a(1)%entries)) error stop 4
  if (.not. allocated(a(1)%entries(1)%path)) error stop 5
  if (a(1)%entries(1)%path /= 'beta') error stop 6
  print '(a,l1,1x,a)', 'elem=', allocated(a(1)%entries), a(1)%entries(1)%path

  print '(a)', 'ok'
end program ar8_selfassign_derived_scalar
