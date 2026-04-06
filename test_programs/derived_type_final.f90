! FINAL procedure test: cleanup called when variable goes out of scope.
! This is where gfortran crashes on ARM64 (automatic finalization bug).
!
! CHECK: alive: 42
! CHECK: cleanup: 42
program test_final
    implicit none
    type :: resource
        integer :: handle
    contains
        final :: cleanup
    end type
    type(resource) :: r
    r%handle = 42
    print *, 'alive:', r%handle
    ! r goes out of scope here — cleanup should be called.
end program

subroutine cleanup(self)
    implicit none
    type :: resource
        integer :: handle
    end type
    type(resource) :: self
    print *, 'cleanup:', self%handle
end subroutine
