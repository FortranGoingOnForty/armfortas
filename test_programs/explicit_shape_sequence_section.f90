! CHECK: 1010
! IR_CHECK: call @afs_create_section
! IR_NOT: call @afs_allocate_like_with_elem_size
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program explicit_shape_sequence_section
  implicit none
  integer :: a(4,6), i, j, got

  do j = 1, 6
    do i = 1, 4
      a(i,j) = 100*j + i
    end do
  end do

  call check(6, a(:,1), got)
  print *, got

contains
  subroutine check(n, x, got)
    integer, value :: n
    integer, intent(in) :: x(4,n)
    integer, intent(out) :: got

    got = x(1,1) + x(4,1) + x(1,2) + x(4,n)
  end subroutine
end program explicit_shape_sequence_section
