! l06: F2023 C_F_STRPOINTER associates a deferred-length c_char pointer
! with a C string. Both forms: rank-1 c_char array (NCHARS optional, else
! array size) and type(c_ptr) (NCHARS required). The length is the longest
! NUL-free prefix bounded by NCHARS or the array size. No copy — the
! pointer aliases the source bytes.
! FLAGS: --std=f2023
program l06_c_f_strpointer
  use, intrinsic :: iso_c_binding
  implicit none
  character(kind=c_char), target :: buf(10)
  character(len=:, kind=c_char), pointer :: s
  type(c_ptr) :: p

  buf(1) = 'h'; buf(2) = 'e'; buf(3) = 'l'; buf(4) = 'l'; buf(5) = 'o'
  buf(6) = c_null_char
  buf(7) = 'X'; buf(8) = 'X'; buf(9) = 'X'; buf(10) = 'X'

  ! Array form, no NCHARS: scan to the NUL.
  call c_f_strpointer(buf, s)
  print '(A,1X,I0)', s, len(s)
  ! CHECK: hello 5

  ! Array form, NCHARS below the NUL: bounded length.
  call c_f_strpointer(buf, s, 3)
  print '(A,1X,I0)', s, len(s)
  ! CHECK: hel 3

  ! c_ptr form, NCHARS=10: address of buf, scan to the NUL.
  p = c_loc(buf)
  call c_f_strpointer(p, s, 10)
  print '(A,1X,I0)', s, len(s)
  ! CHECK: hello 5

  ! Aliasing: writing through the pointer changes the source (no copy).
  call c_f_strpointer(buf, s)
  s(1:1) = 'J'
  print '(A1)', buf(1)
  ! CHECK: J

  ! DEALLOCATE of an aliasing pointer must not free the source storage.
  deallocate (s)
  print '(A)', 'ok'
  ! CHECK: ok
  ! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|exit
end program l06_c_f_strpointer
