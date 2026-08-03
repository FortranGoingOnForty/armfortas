! DEFAULT(NONE) requires every enclosing variable referenced in the
! do-concurrent-block to appear in a locality-spec.
!
! FLAGS: --std=f2023
! ERROR_EXPECTED: DEFAULT(NONE)
program ar43_do_concurrent_default_none_missing
  implicit none
  integer :: i, seed
  integer :: result(2)
  seed = 10
  result = 0
  do concurrent (i = 1:2) default(none)
    result(i) = seed + i
  end do
end program ar43_do_concurrent_default_none_missing
