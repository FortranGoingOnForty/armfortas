! Regression: generic dispatch on an allocatable rank-2 derived-type
! component. The component `index(:,:)` is deferred-shape, so its layout
! carries empty dims; the declared rank (2) was lost, so the actual
! reported rank 1 and no rank-2 specific matched — a "no specific
! procedure of generic 'sort_coo' matches" error. This is the stdlib
! sparse `sort_coo(COO%index, ...)` regression, minimized. Correct
! dispatch binds the 4-arg specific and computes 5 + 7 + 0 = 12.
!
! CHECK: 12
module gd_arc
  implicit none
  integer, parameter :: ilp = 4
  type :: base
    integer(ilp) :: nrows = 0, ncols = 0, nnz = 0
  end type
  type, extends(base) :: coo_t
    integer(ilp), allocatable :: index(:,:)
  end type
  interface sort_coo
    module procedure sort4
    module procedure sort5
  end interface
contains
  subroutine sort4(a, n, num_rows, num_cols)
    integer(ilp), intent(inout) :: a(2,*)
    integer(ilp), intent(inout) :: n
    integer(ilp), intent(in) :: num_rows, num_cols
    n = num_rows + num_cols + a(1,1)
  end subroutine
  subroutine sort5(a, data, n, num_rows, num_cols)
    integer(ilp), intent(inout) :: a(2,*)
    real, intent(inout) :: data(*)
    integer(ilp), intent(inout) :: n
    integer(ilp), intent(in) :: num_rows, num_cols
    n = num_rows
  end subroutine
end module

program generic_dispatch_allocatable_rank2_component
  use gd_arc
  implicit none
  type(coo_t) :: c
  allocate(c%index(2,10))
  c%index = 0
  c%nnz = 3
  c%nrows = 5
  c%ncols = 7
  call sort_coo(c%index, c%nnz, c%nrows, c%ncols)   ! 4-arg -> sort4
  print '(i0)', c%nnz                               ! 5 + 7 + 0 = 12
end program generic_dispatch_allocatable_rank2_component
