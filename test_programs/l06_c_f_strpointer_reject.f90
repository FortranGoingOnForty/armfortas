! l06 boundary: the c_ptr form of C_F_STRPOINTER carries no size, so the
! byte count cannot be inferred — NCHARS is required (F2023 18.2.3.5). A
! call omitting it is rejected loudly.
! FLAGS: --std=f2023
! ERROR_EXPECTED: NCHARS is required when the source is a type(c_ptr)
program l06_c_f_strpointer_reject
  use, intrinsic :: iso_c_binding
  implicit none
  character(len=:, kind=c_char), pointer :: s
  type(c_ptr) :: p
  call c_f_strpointer(p, s)
  print *, s
end program l06_c_f_strpointer_reject
