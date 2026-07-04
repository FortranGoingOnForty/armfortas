! Regression: a polymorphic allocatable dummy with intent(out) must be
! deallocated on entry (becoming unallocated), NOT component-default-
! initialized through its data pointer. armfortas previously ran the
! derived-type default-init on entry, writing through the now-NULL data
! pointer of the unallocated descriptor -> SIGSEGV. Surfaced building
! toml-f (tomlf_type::new_keyval_, a `class(toml_value), allocatable,
! intent(out)` dummy); x12 campaign.
!
! CHECK: empty ok
! CHECK: allocated leaf 7
module x12_caio
  implicit none
  type, abstract :: base
    integer :: tag = 0
  end type
  type, extends(base) :: leaf
    integer :: n = 0
  end type
contains
  ! Empty body: the bug was purely the intent(out) entry reset.
  subroutine reset_it(self)
    class(base), allocatable, intent(out) :: self
  end subroutine

  ! Allocate-and-init through the polymorphic allocatable out dummy.
  subroutine make_leaf(self, v)
    class(base), allocatable, intent(out) :: self
    integer, intent(in) :: v
    allocate(leaf :: self)
    select type(self)
    type is (leaf)
      self%n = v
    end select
  end subroutine
end module

program main
  use x12_caio
  implicit none
  class(base), allocatable :: a, b

  call reset_it(a)
  if (.not. allocated(a)) print *, 'empty ok'   ! deallocated on entry

  call make_leaf(b, 7)
  select type(b)
  type is (leaf)
    print *, 'allocated leaf', b%n
  end select
end program
