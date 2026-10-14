! Type-bound procedure (method) call.
! Tests: call obj%method() with PASS semantics (obj as first arg).
!
! CHECK: 10
module counter_m
    implicit none
    type :: counter
        integer :: val
    contains
        procedure :: show => show_counter
    end type
contains
    subroutine show_counter(self)
        class(counter), intent(in) :: self
        print *, self%val
    end subroutine
end module

program test_method
    use counter_m, only: counter
    implicit none
    type(counter) :: c
    c%val = 10
    call c%show()
end program
