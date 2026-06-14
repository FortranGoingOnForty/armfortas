! l02a item 2: pointer bounds remapping from an array constructor
! (F2023 10.2.2.2). `q([2,3]) => t` views the rank-1 target t as a 2x3
! array, the same as `q(1:2, 1:3) => t`. Column-major: q(2,1)=t(2),
! q(1,2)=t(3), q(2,3)=t(6). Rejected loudly until the remap lowering landed.
! FLAGS: --std=f2023
program l02a_pointer_remap_array_bounds
  implicit none
  integer, target :: t(6)
  integer, pointer :: q(:, :)
  t = [1, 2, 3, 4, 5, 6]
  q([2, 3]) => t
  print '(I0,1X,I0)', shape(q)
  ! CHECK: 2 3
  print '(I0)', q(2, 1)
  ! CHECK: 2
  print '(I0)', q(1, 2)
  ! CHECK: 3
  print '(I0)', q(2, 3)
  ! CHECK: 6
  ! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|exit
end program l02a_pointer_remap_array_bounds
