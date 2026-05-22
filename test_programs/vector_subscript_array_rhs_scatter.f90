! CHECK: ok
! IR_CHECK: vec_subscript_body
! IR_NOT: ptr_to_int
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program vector_subscript_array_rhs_scatter
  implicit none
  integer(8), parameter :: dim_range(3) = [1_8, 2_8, 3_8]
  integer(8) :: iperm(3), perm(3), spack(3), s(3)

  s = [3_8, 2_8, 4_8]
  perm = [2_8, pack(dim_range, dim_range /= 2_8)]
  iperm = -9_8
  iperm(perm) = dim_range
  spack = s(perm)

  if (any(iperm /= [2_8, 1_8, 3_8])) error stop 1
  if (any(spack /= [2_8, 3_8, 4_8])) error stop 2

  print *, 'ok'
end program vector_subscript_array_rhs_scatter
