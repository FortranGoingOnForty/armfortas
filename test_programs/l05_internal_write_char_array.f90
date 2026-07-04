! Whole character arrays as internal WRITE units get record-per-
! element semantics (F2023 12.6.4.8.3): each format reversion starts
! the next element, records are blank-padded to the element length,
! and elements past the last record stay unchanged. Previously both
! fixed and allocatable array units silently wrote nothing (len-0
! buffer view). Overflow reports through IOSTAT= when present.
program l05_internal_write_char_array
  implicit none
  character(len=5) :: c(3)
  character(len=:), allocatable :: a(:)
  character(len=8) :: two(2)
  character(len=4) :: keep(3)
  integer :: ios

  ! Fixed array: '(i0)' reversion = one record per value.
  write(c, '(i0)') 11, 22, 33
  print '(a,a,a)', '<', c(1), '>'
  print '(a,a,a)', '<', c(2), '>'
  print '(a,a,a)', '<', c(3), '>'
! CHECK: <11   >
! CHECK: <22   >
! CHECK: <33   >

  ! Allocated deferred-length array.
  allocate(character(len=4) :: a(2))
  write(a, '(i0)') 7, 8
  print '(a,a,a)', '[', a(1), ']'
  print '(a,a,a)', '[', a(2), ']'
! CHECK: [7   ]
! CHECK: [8   ]

  ! A format consuming two values per scan packs two per record.
  write(two, '(i3,i3)') 1, 2, 3, 4
  print '(a,a,a)', '(', two(1), ')'
  print '(a,a,a)', '(', two(2), ')'
! CHECK: (  1  2  )
! CHECK: (  3  4  )

  ! Elements past the last record written are left unchanged.
  keep(1) = 'aaaa'
  keep(2) = 'bbbb'
  keep(3) = 'cccc'
  write(keep, '(i0)') 9
  print '(a,a,a)', '{', keep(1), '}'
  print '(a,a,a)', '{', keep(2), '}'
  print '(a,a,a)', '{', keep(3), '}'
! CHECK: {9   }
! CHECK: {bbbb}
! CHECK: {cccc}

  ! Overflow: five records into three elements reports via IOSTAT=.
  ios = 0
  write(c, '(i0)', iostat=ios) 1, 2, 3, 4, 5
  print '(a,i0)', 'ios=', merge(1, 0, ios /= 0)
! CHECK: ios=1
end program l05_internal_write_char_array
