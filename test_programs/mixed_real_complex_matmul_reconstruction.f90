! CHECK: 1
! IR_CHECK: array_promote_complex_check
! IR_CHECK: call @afs_matmul_complex
! IR_NOT: call @afs_matmul_real8
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program mixed_real_complex_matmul_reconstruction
  implicit none
  real, parameter :: sqrt2 = sqrt(2.0)
  complex, parameter :: cone = (1.0, 0.0)
  complex, parameter :: cimg = (0.0, 1.0)
  complex, parameter :: amat(2,2) = reshape([cone,cimg,cimg,cone], [2,2])
  complex :: u(2,2), vt(2,2), tmp(2,2), recon(2,2)
  real :: d(2,2), s(2)

  s = [sqrt2, sqrt2]
  d = 0.0
  d(1,1) = s(1)
  d(2,2) = s(2)
  u = reshape([(-0.70710677, 0.0), (0.0, 0.70710683), &
               (-0.70710683, 0.0), (0.0, -0.70710677)], [2,2])
  vt = reshape([(0.0, 0.0), (-1.0, 0.0), &
                (0.0, -1.0), (0.0, 0.0)], [2,2])

  tmp = matmul(d, vt)
  recon = matmul(u, tmp)
  if (all(abs(recon - amat) <= 1.0e-5)) then
    print *, 1
  else
    print *, 0
  end if
end program mixed_real_complex_matmul_reconstruction
