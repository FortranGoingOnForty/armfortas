! CHECK: 5 20 47 46
! IR_CHECK: const_float 47
! IR_NOT: call @afs_matmul
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program parameter_matmul_matrix_vector
  implicit none
  integer, parameter :: wp = kind(1.0)
  integer, parameter :: ndim = 4
  real(wp), parameter :: dense(ndim,ndim) = reshape(real([1,2,0,0, &
                                                          2,3,4,0, &
                                                          0,4,5,6, &
                                                          0,0,6,7], kind=wp), [ndim,ndim])
  real(wp), parameter :: vec_x(ndim) = real([1,2,3,4], kind=wp)
  real(wp), parameter :: vec_y_ref(ndim) = matmul(dense, vec_x)

  print *, int(vec_y_ref)
end program
