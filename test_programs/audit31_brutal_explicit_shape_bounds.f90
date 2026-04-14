! audit31: explicit-shape dummy xs(n), where n is a dummy argument,
! used to emit bounds check (1,1) instead of (1,n) because
! arg_dims_from_decls fell back when the upper-bound expr wasn't
! const-foldable. install_runtime_dim_bounds now lowers the bound
! at function entry; compute_flat_elem_offset consults it for the
! bounds-check upper and cumulative stride. Task #483.
! CHECK: 150
program audit31_explicit_shape_bounds
  implicit none
  integer :: xs(5), total
  xs = (/10, 20, 30, 40, 50/)
  call sum5(xs, 5, total)
  print *, total
contains
  subroutine sum5(xs, n, r)
    integer, intent(in) :: n, xs(n)
    integer, intent(out) :: r
    integer :: i
    r = 0
    do i = 1, n
      r = r + xs(i)
    end do
  end subroutine
end program
