program ar12_slashstar_in_string
  implicit none
  integer :: x
  character(len=*), parameter :: marker_open = '/*'
  character(len=*), parameter :: marker_close = '*/'

  x = 1        ! /*
  x = x + 41
  ! */
  print '(i0)', x
! CHECK: 42
  print '(a,1x,a)', marker_open, marker_close
! CHECK: /* */
  print '(a)', 'literal /* kept */'
! CHECK: literal /* kept */
end program ar12_slashstar_in_string
