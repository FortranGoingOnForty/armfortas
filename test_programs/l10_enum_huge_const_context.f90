! l10 residual closed: HUGE of an enumeration value in a CONSTANT
! context (parameter initializer) folded through the IR type to
! i32::MAX. The const evaluator now answers the last enumerator.
! FLAGS: --std=f2023
program l10_enum_huge_const_context
  implicit none
  enumeration type :: color
    enumerator :: red, green, blue
  end enumeration type
  type(color) :: c
  integer, parameter :: h = huge(c)
  print '(i0)', h
! CHECK: 3
end program l10_enum_huge_const_context
