! Implicit SAVE: a local variable with an initializer in a
! subprogram has SAVE attribute per F2018 §8.5.16. Its value must
! persist across calls.
!
! Before the fix, init_decls re-stored the initializer on every
! call, defeating SAVE and printing 1, 1, 1, 1, 1.
!
! Audit MAJOR-1.
!
! CHECK: 1
! CHECK: 2
! CHECK: 3
! CHECK: 4
! CHECK: 5
program save_attribute
  call show()
  call show()
  call show()
  call show()
  call show()
contains
  subroutine show()
    integer :: count = 0
    count = count + 1
    print *, count
  end subroutine
end program
