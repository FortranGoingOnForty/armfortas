! CHECK: ok
! IR_CHECK: func @afs_internal___prog_stdlib_host_large_array_section_1(%0: ptr<[i8 x 384]>)
! IR_CHECK: call @afs_create_section(
! REPRO_CHECK: run
program stdlib_host_large_array_section
  implicit none
  integer, parameter :: n = 70000
  integer(1) :: values(n)

  values = 0_1
  values(1) = 11_1
  values(n) = 22_1
  call inner()

contains
  subroutine inner()
    if (first(values(1:1)) /= 11) error stop 1
    if (first(values(n:n)) /= 22) error stop 2
    print *, 'ok'
  end subroutine

  integer function first(x) result(y)
    integer(1), intent(in) :: x(:)

    y = x(1)
  end function
end program
