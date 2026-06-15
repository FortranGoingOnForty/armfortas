! l04: F2023 SELECTED_LOGICAL_KIND(BITS) — smallest logical kind with
! at least BITS bits (8/16/32/64/128 → kinds 1/2/4/8/16), -1 if none.
! Constant-folded in sema (usable as a kind parameter) and available at
! runtime for non-constant arguments; held identical across opt levels.
! FLAGS: --std=f2023
! CHECK: k1 1
! CHECK: k2 2
! CHECK: k4 4
! CHECK: k8 8
! CHECK: k16 16
! CHECK: none -1
! CHECK: param 4
! CHECK: rt T
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program l04_selected_logical_kind
  implicit none
  integer, parameter :: kp = selected_logical_kind(32)
  logical(kind=kp) :: flag
  integer :: i

  print '(A,1X,I0)', 'k1', selected_logical_kind(8)
  print '(A,1X,I0)', 'k2', selected_logical_kind(16)
  print '(A,1X,I0)', 'k4', selected_logical_kind(32)
  print '(A,1X,I0)', 'k8', selected_logical_kind(64)
  print '(A,1X,I0)', 'k16', selected_logical_kind(128)
  print '(A,1X,I0)', 'none', selected_logical_kind(129)

  ! Constant-folded into a kind parameter.
  print '(A,1X,I0)', 'param', kind(flag)

  ! Non-constant argument → runtime path.
  i = 9
  print '(A,1X,L1)', 'rt', selected_logical_kind(i) == 2

  print '(A)', 'ok'
end program l04_selected_logical_kind
