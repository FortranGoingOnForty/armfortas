! l03: F2023 enumeration types (7.6.2) — two distinct types,
! constructor R771, INT() of an enumerator ordinal, all six relational
! ops over the same type (ordinal order), SELECT CASE with an
! enumerator range (the 7.6.2 NOTE shape).
! FLAGS: --std=f2023
! CHECK: ord 1 2 3
! CHECK: eq T
! CHECK: ne T
! CHECK: lt T
! CHECK: le T
! CHECK: gt T
! CHECK: ge T
! CHECK: ctor 2
! CHECK: case gb
! CHECK: fruit 1
! CHECK: ok
program l03_enumeration_basic
  implicit none
  enumeration type :: color
    enumerator :: red, green, blue
  end enumeration type
  enumeration type :: fruit
    enumerator :: apple, pear
  end enumeration type
  type(color) :: c, d
  type(fruit) :: f

  print '(A,1X,I0,1X,I0,1X,I0)', 'ord', int(red), int(green), int(blue)

  c = red
  d = blue
  print '(A,1X,L1)', 'eq', c == red
  print '(A,1X,L1)', 'ne', c /= d
  print '(A,1X,L1)', 'lt', c < d
  print '(A,1X,L1)', 'le', c <= red
  print '(A,1X,L1)', 'gt', d > c
  print '(A,1X,L1)', 'ge', d >= blue

  c = color(2)
  print '(A,1X,I0)', 'ctor', int(c)

  c = green
  select case (c)
  case (red)
    print '(A)', 'case r'
  case (green:blue)
    print '(A)', 'case gb'
  end select

  f = apple
  print '(A,1X,I0)', 'fruit', int(f)
  print '(A)', 'ok'
end program l03_enumeration_basic
