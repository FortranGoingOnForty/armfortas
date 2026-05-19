! CHECK: ok
! IR_CHECK: call @afs_copy_array_data
! REPRO_CHECK: run
! PHASE_TRIANGULATE: ir|asm|obj|repro
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program negative_stride_section_overlap
  implicit none
  integer :: x(10)
  integer :: i

  x = (/( i**2, i=1, size(x) )/)
  x(5:2:-1) = x(2:5)
  x(10:8:-1) = x(8:10)

  if (any(x /= [1, 25, 16, 9, 4, 36, 49, 100, 81, 64])) error stop 1
  print *, 'ok'
end program negative_stride_section_overlap
