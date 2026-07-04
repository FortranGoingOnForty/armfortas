! NEXT past the last enumerator without STAT= is a runtime error
! (F2023 16.9.148 makes the result undefined; silence would be the
! wrong-answer class).
! FLAGS: --std=f2023
! STDERR_CHECK: out of range
! EXIT_CODE: 1
program ltail_enum_next_overflow_loud
  implicit none
  enumeration type :: color
    enumerator :: red, green, blue
  end enumeration type
  type(color) :: c
  c = blue
  c = next(c)
  print '(a)', 'unreachable'
end program ltail_enum_next_overflow_loud
