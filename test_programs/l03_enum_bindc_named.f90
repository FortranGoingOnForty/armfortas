! l03: F2023 named interoperable ENUM (R760) — the enum-type-name is a
! weak alias for the companion-processor integer type: TYPE(name)
! declares an integer, enumerators keep explicit values, and integer
! assignment in both directions stays legal (contrast ENUMERATION
! TYPE, which is a distinct TKR).
! FLAGS: --std=f2023
! CHECK: n 10
! CHECK: s 15
! CHECK: ok
program l03_enum_bindc_named
  implicit none
  enum, bind(c) :: speed
    enumerator :: slow = 10, fast = 20
  end enum
  type(speed) :: s
  integer :: n
  s = slow
  n = s
  s = 15
  print '(A,1X,I0)', 'n', n
  print '(A,1X,I0)', 's', s
  print '(A)', 'ok'
end program l03_enum_bindc_named
