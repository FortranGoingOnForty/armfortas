! Regression: PRINT with a character format string must use that format,
! not fall back to list-directed. The Print lowering ignored its format
! field and always emitted list-directed output, so `print '(a,i0)',
! 'x=', 9` wrote ` x= 9` and `print '("lit",i3)', 7` dropped the literal
! entirely. PRINT fmt now routes through the same formatted machinery as
! WRITE(*, fmt). Numeric FORMAT labels are covered separately.
! The CHECK lines below would not match the old list-directed spacing. x12.
!
! CHECK: x=9
! CHECK: lit  7
! CHECK: v=5
program t
  implicit none
  character(len=8) :: fmt
  fmt = '(a,i0)'
  print '(a,i0)', 'x=', 9
  print '("lit",i3)', 7
  print fmt, 'v=', 5
end program t
