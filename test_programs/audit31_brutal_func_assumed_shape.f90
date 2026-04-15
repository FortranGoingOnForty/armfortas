! audit31 Finding 6: a FUNCTION taking `xs(:)` received a raw
! element pointer instead of a descriptor because lower_expr_full
! didn't consult the descriptor_params mask (only the Stmt::Call
! path did). size(xs) then read zeros out of the raw pointer.
! Thread descriptor_params as an optional arg through
! lower_expr_full and emit lower_arg_descriptor for any callee
! param flagged as descriptor-backed. Task #487.
! CHECK: 6
program audit31_func_assumed
  implicit none
  integer :: data(6), i
  do i = 1, 6
    data(i) = i
  end do
  print *, arr_size(data)
contains
  function arr_size(xs) result(r)
    integer, intent(in) :: xs(:)
    integer :: r
    r = size(xs)
  end function
end program
