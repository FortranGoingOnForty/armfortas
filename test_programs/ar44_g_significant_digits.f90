! Nonzero-width G editing uses d significant digits, selects its editing
! style after rounding, and reserves the exponent columns in fixed form.
!
! FLAGS: --std=f2018
! CHECK: ok
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: call @afs_fmt_push_real(
! IR_CHECK: call @afs_fmt_push_real32(
program ar44_g_significant_digits
  implicit none

  call check_g12(0.0_8, '   0.000    ', 1)
  call check_g12(0.012345_8, '  0.1235E-01', 2)
  call check_g12(0.1_8, '  0.1000    ', 3)
  call check_g12(1.2345_8, '   1.234    ', 4)
  call check_g12(12.345_8, '   12.35    ', 5)
  call check_g12(1234.5_8, '   1234.    ', 6)
  call check_g12(9999.5_8, '  0.1000E+05', 7)
  call check_g12(0.099995_8, '  0.1000    ', 8)
  call check_g12(-12.345_8, '  -12.35    ', 9)

  call check_g14e3(12.345_8, '    12.35     ', 10)
  call check_g14e3(0.012345_8, '   0.1235E-001', 11)
  call check_g12e0(12.345_8, '   12.35    ', 12)
  call check_g12e0(0.012345_8, '   0.1235E-1', 13)
  call check_g8(12.345_8, '********', 14)

  call check_ru_small(0.099991_8, '  0.1000    ', 15)
  call check_rd_small(0.099991_8, '  0.9999E-01', 16)
  call check_sp(12.345_8, '  +12.35    ', 17)
  call check_dc(12.345_8, '   12,35    ', 18)
  call check_g12_real32(12.345_4, '   12.35    ', 19)

  print '(a)', 'ok'

contains

  subroutine check_g12(value, expected, code)
    real(kind=8), intent(in) :: value
    character(len=*), intent(in) :: expected
    integer, intent(in) :: code
    character(len=12) :: record

    write(record, '(G12.4)') value
    if (record /= expected) error stop code
  end subroutine check_g12

  subroutine check_g14e3(value, expected, code)
    real(kind=8), intent(in) :: value
    character(len=*), intent(in) :: expected
    integer, intent(in) :: code
    character(len=14) :: record

    write(record, '(G14.4E3)') value
    if (record /= expected) error stop code
  end subroutine check_g14e3

  subroutine check_g12e0(value, expected, code)
    real(kind=8), intent(in) :: value
    character(len=*), intent(in) :: expected
    integer, intent(in) :: code
    character(len=12) :: record

    write(record, '(G12.4E0)') value
    if (record /= expected) error stop code
  end subroutine check_g12e0

  subroutine check_g8(value, expected, code)
    real(kind=8), intent(in) :: value
    character(len=*), intent(in) :: expected
    integer, intent(in) :: code
    character(len=8) :: record

    write(record, '(G8.4)') value
    if (record /= expected) error stop code
  end subroutine check_g8

  subroutine check_ru_small(value, expected, code)
    real(kind=8), intent(in) :: value
    character(len=*), intent(in) :: expected
    integer, intent(in) :: code
    character(len=12) :: record

    write(record, '(RU,G12.4)') value
    if (record /= expected) error stop code
  end subroutine check_ru_small

  subroutine check_rd_small(value, expected, code)
    real(kind=8), intent(in) :: value
    character(len=*), intent(in) :: expected
    integer, intent(in) :: code
    character(len=12) :: record

    write(record, '(RD,G12.4)') value
    if (record /= expected) error stop code
  end subroutine check_rd_small

  subroutine check_sp(value, expected, code)
    real(kind=8), intent(in) :: value
    character(len=*), intent(in) :: expected
    integer, intent(in) :: code
    character(len=12) :: record

    write(record, '(SP,G12.4)') value
    if (record /= expected) error stop code
  end subroutine check_sp

  subroutine check_dc(value, expected, code)
    real(kind=8), intent(in) :: value
    character(len=*), intent(in) :: expected
    integer, intent(in) :: code
    character(len=12) :: record

    write(record, '(DC,G12.4)') value
    if (record /= expected) error stop code
  end subroutine check_dc

  subroutine check_g12_real32(value, expected, code)
    real(kind=4), intent(in) :: value
    character(len=*), intent(in) :: expected
    integer, intent(in) :: code
    character(len=12) :: record

    write(record, '(G12.4)') value
    if (record /= expected) error stop code
  end subroutine check_g12_real32

end program ar44_g_significant_digits
