! Audit C6: character array constructors mis-stored their elements. Two bugs,
! both here:
!  (1) A character(len>1) array element ([g, ...] where g is a character(2)
!      array) reached isel as an [i8 x N] value in a register. x86 isel has a
!      register class only for the 8-byte and wide-pair array sizes, so len=2
!      panicked with "no register class for Array(Int(I8), 2)"; arm64 emitted a
!      single 8-byte copy (silent truncation). The whole-array element copy now
!      memcpys every aggregate element regardless of size.
!  (2) A character(len=1) scalar element ('w') lowered to a Ptr<i8> at the
!      source character. coerce_to_type(Ptr, i8) emits ptr_to_int + int_trunc,
!      which stored the pointer's low address byte instead of the character
!      (silent corruption -> garbage that panicked afs_fmt_end). The scalar-
!      element path now loads the byte from the pointer.
program char_array_constructor
  character(len=3) :: c(2)
  character(len=3), allocatable :: e(:)
  character(len=2) :: g(2)
  character(len=2), allocatable :: h(:)
  character(len=1) :: d(3)
  character(len=1), allocatable :: f(:)

  c(1) = 'foo'; c(2) = 'bar'
  ! whole character(3) array + scalar literal + array element
  e = [c, 'baz', c(1)]
  print '(A,I0)', 'ne=', size(e)
  ! CHECK: ne=4
  print '(A,4(A3,1X))', 'e=', e
  ! CHECK: e=foo bar baz foo

  g(1) = 'ab'; g(2) = 'cd'
  ! character(len=2): the original ICE ("no register class for Array(I8,2)")
  h = [g, 'ef']
  print '(A,I0)', 'nh=', size(h)
  ! CHECK: nh=3
  print '(A,3(A2,1X))', 'h=', h
  ! CHECK: h=ab cd ef

  d = ['x', 'y', 'z']
  ! character(len=1) scalar element used to store the pointer's low byte
  f = [d, 'w']
  print '(A,I0)', 'nf=', size(f)
  ! CHECK: nf=4
  print '(A,4(A1,1X))', 'f=', f
  ! CHECK: f=x y z w
end program
