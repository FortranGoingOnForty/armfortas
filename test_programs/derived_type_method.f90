! Type-bound procedure (method) call.
! Tests: call obj%method() with PASS semantics (obj as first arg).
!
! CHECK: 10
program test_method
    implicit none
    type :: counter
        integer :: val
    contains
        procedure :: show => show_counter
    end type
    type(counter) :: c
    c%val = 10
    call c%show()
end program

subroutine show_counter(self)
    implicit none
    type :: counter
        integer :: val
    end type
    type(counter) :: self
    print *, self%val
end subroutine
