! CHECK: ok
! IR_CHECK: afs_modproc_m_default_fn
! REPRO_CHECK: run
module m
  implicit none

  abstract interface
    function ifn(x) result(r)
      integer, intent(in) :: x
      integer :: r
    end function ifn
  end interface

  type, abstract :: parent_t
    procedure(ifn), pointer, nopass :: fn => default_fn
  end type parent_t

  type, extends(parent_t) :: child_t
    integer :: marker = 0
  end type child_t

contains

  function default_fn(x) result(r)
    integer, intent(in) :: x
    integer :: r

    r = x * 3
  end function default_fn

  subroutine init_child(self)
    class(child_t), intent(out) :: self

    self%marker = 7
  end subroutine init_child
end module m

program p
  use m
  implicit none

  type(child_t) :: child
  integer :: got

  call init_child(child)
  if (.not. associated(child%fn)) error stop 1

  got = child%fn(5)
  if (got /= 15) error stop 2
  if (child%marker /= 7) error stop 3

  print *, 'ok'
end program p
