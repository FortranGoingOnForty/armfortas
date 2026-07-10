! Fixed-length CHARACTER variables in an EQUIVALENCE group share
! inline byte storage, including character arrays and mixed storage.
!
! CHECK: c2 [cd]
! CHECK: cv [ZYcdefgh]
! CHECK: scalar [WXYZ]
! CHECK: int_from_char 67305985
! CHECK: char_from_int [ABCD]
program ar5_equiv_char
  use, intrinsic :: iso_fortran_env, only: int32
  implicit none
  character(8) :: cv
  character(2) :: c2(4)
  character(4) :: a, b
  character(4) :: raw
  integer(int32) :: iv

  equivalence (cv, c2)
  equivalence (a, b)
  equivalence (raw, iv)

  cv = 'abcdefgh'
  print '(3a)', 'c2 [', c2(2), ']'
  c2(1) = 'ZY'
  print '(3a)', 'cv [', cv, ']'

  a = 'WXYZ'
  print '(3a)', 'scalar [', b, ']'

  raw = achar(1)//achar(2)//achar(3)//achar(4)
  print '(a,i0)', 'int_from_char ', iv
  iv = 1145258561_int32
  print '(3a)', 'char_from_int [', raw, ']'
end program ar5_equiv_char
