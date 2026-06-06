! CHECK: ok
! PHASE_TRIANGULATE: ir|asm|obj|repro
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2 => stdout|exit
program matmul_matrix_vector_result_shape
    implicit none
    real, allocatable :: a(:,:), x(:), y(:), z(:)

    allocate(a(4,5), source = reshape(real([9,4, 0,4, &
                                            0,7, 8,0, &
                                            0,0,-1,5, &
                                            0,0, 8,6, &
                                           -3,0, 0,0]), [4,5]))

    allocate(x(5), source = 1.0)
    y = matmul(a, x)
    if (size(y) /= 4) error stop 1
    if (lbound(y, 1) /= 1) error stop 2
    if (ubound(y, 1) /= 4) error stop 3
    if (any(y /= [6.0, 11.0, 15.0, 15.0])) error stop 4

    deallocate(x)
    allocate(x(4), source = 1.0)
    z = matmul(x, a)
    if (size(z) /= 5) error stop 5
    if (lbound(z, 1) /= 1) error stop 6
    if (ubound(z, 1) /= 5) error stop 7
    if (any(z /= [17.0, 15.0, 4.0, 14.0, -3.0])) error stop 8

    print *, "ok"
end program
