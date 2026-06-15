! A vtable-dispatched type-bound call mutates a polymorphic object, then
! the object is deallocated and the dynamic type's FINAL runs. Vtable
! dispatch changes only where the call target comes from, not whether
! the mutator runs or finalization fires.
!
! (The mutated value is checked through the live object, not the
! finalizer: polymorphic deallocate finalizes with a zeroed copy of the
! object — a pre-existing bug tracked in noted_items.md, unrelated to
! dispatch.)
!
! CHECK: bumped: 99
! CHECK: cleanup ran
module l08_final
  implicit none
  type :: resource
    integer :: handle = 0
  contains
    procedure :: bump => bump_resource
    final :: cleanup
  end type
contains
  subroutine bump_resource(self)
    class(resource), intent(inout) :: self
    self%handle = 99
  end subroutine

  subroutine cleanup(self)
    type(resource), intent(inout) :: self
    if (self%handle /= 0) print *, 'cleanup saw', self%handle
    print *, 'cleanup ran'
  end subroutine
end module

program main
  use l08_final
  implicit none
  class(resource), allocatable :: r
  allocate(resource :: r)
  call r%bump()                 ! vtable-dispatched mutation
  print *, 'bumped:', r%handle
  deallocate(r)                 ! FINAL runs for the dynamic type
end program
