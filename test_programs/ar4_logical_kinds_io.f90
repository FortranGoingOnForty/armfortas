program ar4_logical_kinds_io
  use iso_c_binding, only: c_bool
  implicit none

  logical(1) :: k1 = .true.
  logical(2) :: k2 = .false.
  logical(4) :: k4 = .true.
  logical(8) :: k8 = .false.
  logical(c_bool) :: kb = .true.

  print '(a)', 'before k1'
  ! CHECK: before k1
  print '(l1)', k1
  ! CHECK: T
  print '(g0)', k1
  ! CHECK: T
  print '(a)', 'after k1'
  ! CHECK: after k1

  print '(a)', 'before k2'
  ! CHECK: before k2
  print '(l1)', k2
  ! CHECK: F
  print '(g0)', k2
  ! CHECK: F
  print '(a)', 'after k2'
  ! CHECK: after k2

  print '(a)', 'before k4'
  ! CHECK: before k4
  print '(l1)', k4
  ! CHECK: T
  print '(g0)', k4
  ! CHECK: T
  print '(a)', 'after k4'
  ! CHECK: after k4

  print '(a)', 'before k8'
  ! CHECK: before k8
  print '(l1)', k8
  ! CHECK: F
  print '(g0)', k8
  ! CHECK: F
  print '(a)', 'after k8'
  ! CHECK: after k8

  print '(a)', 'before cb'
  ! CHECK: before cb
  print '(l1)', kb
  ! CHECK: T
  print '(g0)', kb
  ! CHECK: T
  print '(a)', 'after cb'
  ! CHECK: after cb
end program
