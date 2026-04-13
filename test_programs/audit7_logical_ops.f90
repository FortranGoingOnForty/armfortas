! Comprehensive logical operator tests: .and., .or., .eqv., .neqv.
! Exercises all four binary logical operators with various truth values.
! CHECK: 6
! CHECK: pass
! CHECK: 0
! CHECK: fail
program test_logical_ops
  implicit none
  logical :: a, b, c
  integer :: pass_count, fail_count

  pass_count = 0
  fail_count = 0

  ! Test .and.
  a = .true.
  b = .true.
  c = a .and. b
  if (c) then
    pass_count = pass_count + 1
  else
    print *, 'FAIL: T .and. T'
    fail_count = fail_count + 1
  end if

  a = .true.
  b = .false.
  c = a .and. b
  if (c) then
    print *, 'FAIL: T .and. F should be F'
    fail_count = fail_count + 1
  else
    pass_count = pass_count + 1
  end if

  ! Test .or.
  a = .false.
  b = .false.
  c = a .or. b
  if (c) then
    print *, 'FAIL: F .or. F should be F'
    fail_count = fail_count + 1
  else
    pass_count = pass_count + 1
  end if

  a = .true.
  b = .false.
  c = a .or. b
  if (c) then
    pass_count = pass_count + 1
  else
    print *, 'FAIL: T .or. F should be T'
    fail_count = fail_count + 1
  end if

  ! Test .eqv.
  a = .true.
  b = .true.
  c = a .eqv. b
  if (c) then
    pass_count = pass_count + 1
  else
    print *, 'FAIL: T .eqv. T should be T'
    fail_count = fail_count + 1
  end if

  a = .true.
  b = .false.
  c = a .neqv. b
  if (c) then
    pass_count = pass_count + 1
  else
    print *, 'FAIL: T .neqv. F should be T'
    fail_count = fail_count + 1
  end if

  print *, 'Logical ops:', pass_count, 'pass', fail_count, 'fail'
end program
