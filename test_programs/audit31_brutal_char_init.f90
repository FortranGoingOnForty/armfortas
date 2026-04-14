! audit31 Finding 3: character variable/parameter initializer used
! to be silently dropped. init_decls had a catch-all `continue` for
! any char_kind != None, so `character(len=5) :: a = 'hello'` left
! the stack buffer zero-initialised. Added a Fixed-len path that
! calls afs_assign_char_fixed to copy the literal with
! space-padding to the declared length. Task #484.
! CHECK: hello
! CHECK: world
program audit31_char_init
  implicit none
  character(len=5)           :: a = 'hello'
  character(len=5), parameter :: b = 'world'
  print *, '[', a, ']'
  print *, '[', b, ']'
end program
