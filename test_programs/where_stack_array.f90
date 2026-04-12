! WHERE / ELSEWHERE on explicit-shape stack arrays. Regression for a
! runtime panic in afs_array_size: the WHERE lowering called
! afs_array_size(first_arr.addr) directly on the raw element buffer,
! reading the rank field out of whatever happened to sit past the end
! of the alloca. For a 3-element i32 array the "rank" came out as 15
! (MAX_RANK) and the descriptor traversal tripped an out-of-bounds
! panic. For a 5-element array it silently returned garbage extents.
program where_stack_array
  implicit none
  integer :: a3(3), b3(3)
  integer :: a5(5), b5(5)

  a3(1) = -1
  a3(2) = 2
  a3(3) = 3
  where (a3 > 0)
    b3 = 99
  elsewhere
    b3 = -99
  end where
  print *, b3

  a5 = [-1, 2, 3, 10, 20]
  where (a5 > 0)
    b5 = 99
  elsewhere
    b5 = -99
  end where
  print *, b5
end program where_stack_array
! CHECK: -99 99 99
! CHECK: -99 99 99 99 99
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
