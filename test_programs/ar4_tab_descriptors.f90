program ar4_tab_descriptors
  implicit none

  character(len=12) :: buf

  buf = '............'
  write(buf, '(a,t3,a)') 'abcdef', 'XY'
  print '(a)', '[' // buf(:6) // ']'
  ! CHECK: [abXYef]

  buf = '............'
  write(buf, '(a,tl3,a)') 'abcdef', 'XY'
  print '(a)', '[' // buf(:6) // ']'
  ! CHECK: [abcXYf]

  buf = '............'
  write(buf, '(a,tr3,a)') 'ab', 'Z'
  print '(a)', '[' // buf(:6) // ']'
  ! CHECK: [ab   Z]
end program
