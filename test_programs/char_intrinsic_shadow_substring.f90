! fortsh parser reduction: an unrelated dummy named `char` must not
! make `char(1)` lower as a substring of that dummy. The bad path made
! `token(i:i) == char(1)` compare against a zero-length string, which
! pads as a blank and skipped quoted spaces in fortsh.
! CHECK: a b
! IR_CHECK: call @afs_char
! REPRO_CHECK: run
! PHASE_TRIANGULATE: ir|asm|obj|repro
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module char_intrinsic_shadow_substring_mod
  implicit none
contains
  integer function find_outside_quotes(str, char) result(pos)
    character(len=*), intent(in) :: str, char
    pos = 0
    if (len_trim(str) > 0 .and. len_trim(char) > 0) pos = 1
  end function find_outside_quotes

  subroutine copy_skip_sentinel(token, out)
    character(len=*), intent(in) :: token
    character(len=:), allocatable, intent(out) :: out
    character(len=len(token)) :: result
    integer :: i, j

    result = ''
    i = 1
    j = 1
    do while (i <= len(token))
      if (token(i:i) == char(1)) then
        i = i + 1
      else
        result(j:j) = token(i:i)
        i = i + 1
        j = j + 1
      end if
    end do
    out = result(:j-1)
  end subroutine copy_skip_sentinel
end module char_intrinsic_shadow_substring_mod

program char_intrinsic_shadow_substring
  use char_intrinsic_shadow_substring_mod
  implicit none
  character(len=:), allocatable :: out

  call copy_skip_sentinel('a b', out)
  if (len(out) /= 3) error stop 1
  if (out /= 'a b') error stop 2
  print *, out
end program char_intrinsic_shadow_substring
