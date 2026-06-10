! Sprint x01 fixture: a bare CHECK paired with an arm64-qualified
! ASM_CHECK on an ARM mnemonic. The runtime assertion holds everywhere;
! the assembly-shape assertion is only made when compiling for arm64.
! CHECK: 12
! ASM_CHECK(arm64): ret
program x01_qualifier_asm
  implicit none
  integer :: i
  i = 5 + 7
  print *, i
end program x01_qualifier_asm
