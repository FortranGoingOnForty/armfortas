! Host association: contained procs read and write a host-local
! array with an array-constructor initializer. Old shortcut tried
! to promote arr to a module-style global and lost the literal
! initializer — closure passing carries the host's live alloca
! address, so the initializer runs normally and both reads and
! writes see the host's storage.
! CHECK: 10 20 30
! CHECK: 10 99 30
program host_local_array
  implicit none
  integer :: arr(3)
  arr = [10, 20, 30]
  call show()
  call modify()
  print *, arr
contains
  subroutine show()
    print *, arr(1), arr(2), arr(3)
  end subroutine
  subroutine modify()
    arr(2) = 99
  end subroutine
end program
