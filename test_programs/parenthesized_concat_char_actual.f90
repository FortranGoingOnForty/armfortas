! CHECK: ok
! IR_CHECK: call @afs_concat(
! IR_CHECK: call @afs_modproc_parenthesized_concat_char_actual_m_check_pair(
! REPRO_CHECK: run
module parenthesized_concat_char_actual_m
  implicit none
contains
  subroutine check_pair(actual, expected)
    character(len=*), intent(in) :: actual
    character(len=*), intent(in) :: expected

    if (actual /= expected) error stop 1
    if (len(actual) /= 7) error stop 2
    if (len(expected) /= 7) error stop 3
  end subroutine check_pair
end module parenthesized_concat_char_actual_m

program parenthesized_concat_char_actual
  use parenthesized_concat_char_actual_m
  implicit none
  character(len=7) :: actual

  actual = 'S33B000'
  call check_pair(actual, ('S33B' // '000'))
  print *, 'ok'
end program parenthesized_concat_char_actual
