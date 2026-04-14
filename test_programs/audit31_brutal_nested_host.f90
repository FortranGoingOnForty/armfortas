! XFAIL: nested CONTAINS host write not propagated — Task #485
! CHECK: 20
! audit31: host association breaks in nested CONTAINS.
! 'outer' correctly sees host_var (10) from program scope.
! 'inner' inside 'outer' reads host_var but its writes don't propagate
! back to the program scope.
! Expected final: 20
! Actual final:   10
program audit31_nested_host
  implicit none
  integer :: host_var = 10
  call outer()
  print *, 'final host_var=', host_var
contains
  subroutine outer()
    call inner()
  contains
    subroutine inner()
      host_var = host_var * 2
    end subroutine
  end subroutine
end program
