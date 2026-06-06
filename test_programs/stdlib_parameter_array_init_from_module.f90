! CHECK: ok
! IR_CHECK: global_addr @afs_mod_stdlib_parameter_array_init_from_module_cs : ptr<[f32 x 2]>
! IR_CHECK: global_addr @afs_mod_stdlib_parameter_array_init_from_module_cs1 : ptr<[f32 x 2]>
! IR_CHECK: global_addr @afs_mod_stdlib_parameter_array_init_from_module_d : ptr<f64>
! IR_CHECK: global_addr @afs_mod_stdlib_parameter_array_init_from_module_d1 : ptr<f64>
! IR_CHECK: float_trunc
! IR_CHECK: call @memcpy
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module stdlib_parameter_array_init_from_module
  implicit none
  real(8), parameter :: d1(5) = [1.0_8, 2.0_8, 3.0_8, 4.0_8, 5.0_8]
  real(8), parameter :: d(4,3) = reshape([ &
      1.0_8, 3.0_8, 5.0_8, 7.0_8, &
      2.0_8, 4.0_8, 6.0_8, 8.0_8, &
      9.0_8, 10.0_8, 11.0_8, 12.0_8], [4,3])
  complex(4), parameter :: cs1(3) = [(1.0, 0.0), (0.0, 2.0), (3.0, 0.0)]
  complex(4), parameter :: cs(2,2) = reshape([(1.0, 0.0), (0.0, 2.0), &
      (3.0, 0.0), (0.0, 4.0)], [2,2])
contains
  subroutine check()
    real(4), parameter :: x1(5) = d1
    real(4), parameter :: x2(4,3) = d
    integer(4), parameter :: i1(5) = d1
    complex(4), parameter :: z1(3) = cs1
    complex(4), parameter :: z2(2,2) = cs

    if (abs(sum(x1) - 15.0_4) > 1.0e-5_4) error stop 1
    if (abs(x2(3,2) - 6.0_4) > 1.0e-5_4) error stop 2
    if (abs(x2(4,3) - 12.0_4) > 1.0e-5_4) error stop 3
    if (sum(i1) /= 15) error stop 4
    if (abs(sum(z1) - (4.0, 2.0)) > 1.0e-5_4) error stop 5
    if (abs(z2(2,2) - (0.0, 4.0)) > 1.0e-5_4) error stop 6
  end subroutine
end module

program main
  use stdlib_parameter_array_init_from_module
  implicit none
  call check()
  print *, "ok"
end program
