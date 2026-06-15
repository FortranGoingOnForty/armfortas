! l06: F2023 F_C_STRING(STRING [, ASIS]) transformational function.
! Returns TRIM(STRING)//C_NULL_CHAR, or STRING//C_NULL_CHAR when ASIS is
! true. The result length includes the terminating NUL, so a C strlen of
! the bytes sees the trimmed (or as-is) content.
! FLAGS: --std=f2023
program l06_f_c_string
  use, intrinsic :: iso_c_binding, only: c_char, c_null_char
  implicit none
  character(len=:), allocatable :: s
  logical :: flag

  ! Default: trailing blanks trimmed, NUL appended.
  s = f_c_string("abc   ")
  print '(I0)', len(s)
  ! CHECK: 4
  print '(A)', s(1:3)
  ! CHECK: abc
  print '(L1)', s(4:4) == c_null_char
  ! CHECK: T

  ! ASIS true: blanks kept, NUL appended.
  s = f_c_string("abc   ", asis=.true.)
  print '(I0)', len(s)
  ! CHECK: 7
  print '(L1)', s(7:7) == c_null_char
  ! CHECK: T

  ! Runtime ASIS selects between trimmed and as-is length.
  flag = .false.
  s = f_c_string("hi  ", asis=flag)
  print '(I0)', len(s)
  ! CHECK: 3
  flag = .true.
  s = f_c_string("hi  ", asis=flag)
  print '(I0)', len(s)
  ! CHECK: 5

  ! Empty content still terminates with a single NUL.
  s = f_c_string("   ")
  print '(I0)', len(s)
  ! CHECK: 1
  print '(L1)', s(1:1) == c_null_char
  ! CHECK: T
  ! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|exit
end program l06_f_c_string
