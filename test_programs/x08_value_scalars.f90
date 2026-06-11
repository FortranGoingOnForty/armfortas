! x08: VALUE-attribute scalars classify per the host ABI (SysV GP vs
! SSE on x86_64, AAPCS64 on arm) — integer kinds, real kinds, i128,
! mutated in the callee to prove copy-in semantics. (character VALUE
! is sema-rejected pending l06 copy-in lowering.)
! CHECK: 43 8
! CHECK: 250 1850
! CHECK: 170141183460469231731687303715884105727
program x08_value_scalars
  implicit none
  integer(16) :: big
  big = 170141183460469231731687303715884105727_16
  call ints(42, 7_8)
  call reals(2.5, 18.5d0)
  print *, value_i128(big) + 1_16
contains
  subroutine ints(a, b)
    integer, value :: a
    integer(8), value :: b
    a = a + 1
    b = b + 1_8
    print *, a, b
  end subroutine ints
  subroutine reals(x, y)
    real, value :: x
    real(8), value :: y
    x = x * 100.0
    y = y * 100.0d0
    print *, int(x), int(y)
  end subroutine reals
  function value_i128(v) result(r)
    integer(16), value :: v
    integer(16) :: r
    v = v - 1_16
    r = v
  end function value_i128
end program x08_value_scalars
