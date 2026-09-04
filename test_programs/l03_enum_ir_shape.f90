! l03: the IR shape — an enumeration variable is a plain `alloca i32`
! and the enumerator compares as a constant integer ordinal. No new IR
! types, no derived-aggregate storage (all enumeration safety is
! frontend-only).
! FLAGS: --std=f2023
! IR_CHECK: alloca i32
! IR_CHECK: icmp eq
! IR_NOT: ptr<[i8
program l03_enum_ir_shape
  implicit none
  enumeration type :: color
    enumerator :: red, green, blue
  end enumeration type
  type(color) :: c
  c = green
  if (c == green) then
    continue
  else
    error stop 1
  end if
end program l03_enum_ir_shape
