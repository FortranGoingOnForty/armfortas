! Host association: contained procedure writes back through a
! host-associated pointer, so the host sees each update. Proves the
! closure-passing ABI threads the same storage (not a copy) to the
! contained proc — three increments produce 3, not 1.
! CHECK: 3
program host_local_write
  implicit none
  integer :: counter
  counter = 0
  call inc()
  call inc()
  call inc()
  print *, counter
contains
  subroutine inc()
    counter = counter + 1
  end subroutine
end program
