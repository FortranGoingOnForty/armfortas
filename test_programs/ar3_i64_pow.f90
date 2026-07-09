program ar3_i64_pow
  implicit none

  integer(8) :: b8, e8, r8
  integer(8) :: a8(4), x8(4), y8(4)
  integer(4) :: b4, e4, r4
  integer(4) :: a4(4), x4(4), y4(4)

  b8 = 100000_8
  e8 = 2_8
  r8 = b8 ** e8
  print '(a,1x,i0)', 'i64_100000_2', r8

  b8 = 2_8
  e8 = 40_8
  r8 = b8 ** e8
  print '(a,1x,i0)', 'i64_2_40', r8

  b8 = 123456789012345678_8
  e8 = 1_8
  r8 = b8 ** e8
  print '(a,1x,i0)', 'i64_big_1', r8

  b8 = 3_8
  e8 = 39_8
  r8 = b8 ** e8
  print '(a,1x,i0)', 'i64_3_39', r8

  b8 = 2_8
  e8 = -3_8
  r8 = b8 ** e8
  print '(a,1x,i0)', 'i64_neg_zero', r8

  b8 = -1_8
  e8 = -3_8
  r8 = b8 ** e8
  print '(a,1x,i0)', 'i64_neg_one_odd', r8

  b8 = -1_8
  e8 = -4_8
  r8 = b8 ** e8
  print '(a,1x,i0)', 'i64_neg_one_even', r8

  b4 = 12
  e4 = 5
  r4 = b4 ** e4
  print '(a,1x,i0)', 'i32_12_5', r4

  b4 = 2
  e4 = -3
  r4 = b4 ** e4
  print '(a,1x,i0)', 'i32_neg_zero', r4

  a8 = [100000_8, 2_8, 3_8, -1_8]
  x8 = [2_8, 40_8, 39_8, -3_8]
  y8 = a8 ** x8
  print '(a,4(1x,i0))', 'array64', y8

  a4 = [12, 2, -1, -1]
  x4 = [5, 10, -3, -4]
  y4 = a4 ** x4
  print '(a,4(1x,i0))', 'array32', y4
end program
! CHECK: i64_100000_2 10000000000
! CHECK: i64_2_40 1099511627776
! CHECK: i64_big_1 123456789012345678
! CHECK: i64_3_39 4052555153018976267
! CHECK: i64_neg_zero 0
! CHECK: i64_neg_one_odd -1
! CHECK: i64_neg_one_even 1
! CHECK: i32_12_5 248832
! CHECK: i32_neg_zero 0
! CHECK: array64 10000000000 1099511627776 4052555153018976267 -1
! CHECK: array32 248832 1024 -1 1
