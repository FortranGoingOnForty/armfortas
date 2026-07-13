! l10: SELECTED_CHAR_KIND with a variable argument linked against an
! undefined symbol (only literals const-folded). Runtime fallback
! added; values match the constant-fold table.
program l10_selected_char_kind
  implicit none
  character(len=12) :: nm
  nm = 'ascii'
  print '(i0)', selected_char_kind(nm)
! CHECK: 1
  nm = 'DEFAULT'
  print '(i0)', selected_char_kind(nm)
! CHECK: 1
  nm = 'ebcdic'
  print '(i0)', selected_char_kind(nm)
! CHECK: -1
  print '(i0)', selected_char_kind('iso_10646')
! CHECK: -1
end program l10_selected_char_kind
