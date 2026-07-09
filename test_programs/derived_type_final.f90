! FINAL procedure test: cleanup called when a block local goes out of scope.
!
! CHECK: alive: 42
! CHECK: cleanup: 42
module derived_type_final_mod
    implicit none
    type :: resource
        integer :: handle
    contains
        final :: cleanup
    end type

contains
subroutine cleanup(self)
    type(resource), intent(inout) :: self
    print *, 'cleanup:', self%handle
end subroutine
end module

program test_final
    use derived_type_final_mod, only: resource
    implicit none
    block
        type(resource) :: r
        r%handle = 42
        print *, 'alive:', r%handle
    end block
end program
