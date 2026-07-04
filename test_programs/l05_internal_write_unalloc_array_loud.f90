! Internal WRITE to an UNALLOCATED deferred-length character array is
! a program error (only deferred-length scalars auto-allocate,
! F2023 12.6.4.8.3). Without IOSTAT= it must fail loudly — silence
! previously produced a garbage-sized array with exit 0.
! STDERR_CHECK: unallocated
! EXIT_CODE: 2
program l05_internal_write_unalloc_array_loud
  implicit none
  character(len=:), allocatable :: a(:)
  write(a, '(i0)') 1, 2, 3
  print '(a)', 'unreachable'
end program l05_internal_write_unalloc_array_loud
