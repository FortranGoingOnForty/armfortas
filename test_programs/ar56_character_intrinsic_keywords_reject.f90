! Malformed character-intrinsic association must fail in semantic analysis;
! lowering must never reinterpret an unknown keyword as a positional actual.
!
! ERROR_EXPECTED: unknown keyword argument 'reverse' in call to 'index'
program ar56_character_intrinsic_keywords_reject
  implicit none
  integer :: runtime_kind

  print *, index(string='a', substring='a', reverse=.true.)
  print *, scan('abc', string='abc', set='a')
  print *, verify(string='abc', set='a', back=1)
  print *, len(string='a', kind=runtime_kind)
  print *, ichar(c='a', kind=3)
end program ar56_character_intrinsic_keywords_reject
