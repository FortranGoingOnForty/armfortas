! l05-1: F2023 leading-zero control (LZ/LZS/LZP) and the AT edit
! descriptor. LZS suppresses the leading zero before the decimal point
! (0.25 -> .25) for F/E/D/G output; LZP and LZ keep the processor
! default (armfortas prints it). AT outputs a character value with
! trailing blanks trimmed (no field width). Delimiters pin exact
! output through the harness's whitespace normalization. OPT_EQ ties
! all levels; the leading-zero descriptors are runtime-parsed so they
! are opt-level invariant.
! FLAGS: --std=f2023
! CHECK: def | 0.250|
! CHECK: lzs |  .250|
! CHECK: lzp | 0.250|
! CHECK: neg |  -.250|
! CHECK: exp |  .2500E+00|
! CHECK: big | 10.250|
! CHECK: dlz | 0.250|
! CHECK: at hi|
! CHECK: blank |
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program x_l05_leading_zero_and_at
  implicit none
  real(8) :: x

  x = 0.25d0
  write(*,'(A,"|",F6.3,"|")') 'def ', x          ! print (default)
  write(*,'(A,"|",LZS,F6.3,"|")') 'lzs ', x       ! suppress
  write(*,'(A,"|",LZP,F6.3,"|")') 'lzp ', x       ! print
  write(*,'(A,"|",LZS,F7.3,"|")') 'neg ', -x      ! -.250
  write(*,'(A,"|",LZS,E11.4,"|")') 'exp ', x      ! .2500E+00
  write(*,'(A,"|",LZS,F7.3,"|")') 'big ', 10.25d0 ! no leading zero
  write(*,'(A,"|",LZ,F6.3,"|")') 'dlz ', x        ! LZ = default = print

  write(*,'(A,1X,AT,"|")') 'at', 'hi   '          ! trimmed
  write(*,'(A,1X,AT,"|")') 'blank', '     '       ! all blank -> empty
  print '(A)', 'ok'
end program x_l05_leading_zero_and_at
